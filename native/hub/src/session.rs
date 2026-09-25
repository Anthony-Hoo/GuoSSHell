//! 会话：每条 SSH 会话一个任务。连接由 [`crate::connect`] 建立；连上后把远端字节
//! 喂进 `DefaultTerminalEngine` → `render` → run 压缩 → `FrameUpdate`，并把 Dart 的
//! 输入、鼠标、选区、粘贴转给引擎与远端。
//!
//! 单任务拥有 transport（它的方法都要 `&mut self`），用 `select!` 同时听远端事件与
//! Dart 命令——这是上游 actor 的形状。会话类信号都带 `session_id`，[`supervisor`]
//! 按它把请求分给对应的会话任务。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use rinf::{DartSignal, RustSignal, RustSignalBinary, debug_print};
use secrecy::SecretString;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::time::{Instant, sleep_until};

use rshell_m0::rshell_core::{
    CellPosition, KeyCode, KeyModifiers, MouseButton, MouseEventKind, RenderFrame, SelectionRange,
    TerminalInput, TerminalMouseEvent, TerminalSize, Viewport,
};
use rshell_m0::rshell_session::{
    DefaultTerminalEngine, SessionTransport, TerminalEngine, TransportEvent,
};

use crate::app::AppContext;
use crate::connect::{self, Abort, Channels, Connected};
use crate::frame_codec::pack_runs;
use crate::local_network;
use crate::settings;
use crate::signals::interaction::InteractionReply;
use crate::signals::{
    ClipboardText, ConnectHint, ConnectRequest, CopyRequest, DisconnectRequest, FailureKind,
    FrameAck, FrameUpdate, InputRequest, MouseRequest, PasteRequest, PerfStats, ResizeRequest,
    SelectionRequest, SelectionState, SessionState, SessionStatus,
};

const NO_CURSOR: i32 = -1;
/// 远端 PTY 最小可行尺寸；Flutter 在极端布局下可能量出 0。
const MIN_COLS: u16 = 2;
const MIN_ROWS: u16 = 2;

pub enum SessionCommand {
    Resize(TerminalSize),
    Disconnect,
    Input(TerminalInput),
    Mouse(TerminalMouseEvent),
    Selection(SelectionRequest),
    Copy,
    Paste(String),
    FrameAck(u32),
}

/// 一个会话任务的收件口。
struct SessionHandle {
    /// 区分同一 `session_id` 先后的两个任务（结束通知只清自己的登记）。
    generation: u64,
    commands: UnboundedSender<SessionCommand>,
    replies: UnboundedSender<InteractionReply>,
}

/// 会话的结局。
struct SessionEnd {
    state: SessionState,
    failure: FailureKind,
    detail: String,
}

impl SessionEnd {
    fn closed(detail: impl Into<String>) -> Self {
        Self {
            state: SessionState::Closed,
            failure: FailureKind::None,
            detail: detail.into(),
        }
    }

    fn failed(failure: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            state: SessionState::Failed,
            failure,
            detail: detail.into(),
        }
    }
}

/// 常驻任务：接住 Dart 的会话类请求，按 `session_id` 转给对应的会话任务。
pub async fn supervisor(context: Arc<AppContext>) {
    let connect_rx = ConnectRequest::get_dart_signal_receiver();
    let resize_rx = ResizeRequest::get_dart_signal_receiver();
    let disconnect_rx = DisconnectRequest::get_dart_signal_receiver();
    let input_rx = InputRequest::get_dart_signal_receiver();
    let mouse_rx = MouseRequest::get_dart_signal_receiver();
    let selection_rx = SelectionRequest::get_dart_signal_receiver();
    let copy_rx = CopyRequest::get_dart_signal_receiver();
    let ack_rx = FrameAck::get_dart_signal_receiver();
    let paste_rx = PasteRequest::get_dart_signal_receiver();
    let reply_rx = InteractionReply::get_dart_signal_receiver();
    let (finished_tx, mut finished_rx) = unbounded_channel::<(u32, u64)>();
    let mut sessions: HashMap<u32, SessionHandle> = HashMap::new();
    let mut generations: u64 = 0;

    let send =
        |sessions: &HashMap<u32, SessionHandle>, session_id: u32, command: SessionCommand| {
            if let Some(handle) = sessions.get(&session_id) {
                let _ = handle.commands.send(command);
            }
        };

    loop {
        tokio::select! {
            pack = connect_rx.recv() => {
                let Some(pack) = pack else { break };
                let request = pack.message;
                let session_id = request.session_id;
                debug_print!("[session {session_id}] connect ({}x{})", request.cols, request.rows);
                if let Some(old) = sessions.remove(&session_id) {
                    let _ = old.commands.send(SessionCommand::Disconnect);
                }
                generations += 1;
                let generation = generations;
                let (commands_tx, commands) = unbounded_channel();
                let (replies_tx, replies) = unbounded_channel();
                sessions.insert(session_id, SessionHandle {
                    generation,
                    commands: commands_tx,
                    replies: replies_tx,
                });
                let context = context.clone();
                let finished = finished_tx.clone();
                tokio::spawn(async move {
                    run_session(context, request, commands, replies).await;
                    let _ = finished.send((session_id, generation));
                });
            }
            finished = finished_rx.recv() => {
                if let Some((session_id, generation)) = finished
                    && sessions.get(&session_id).is_some_and(|handle| handle.generation == generation)
                {
                    sessions.remove(&session_id);
                }
            }
            pack = resize_rx.recv() => {
                let Some(pack) = pack else { break };
                let request = pack.message;
                let size = clamped_size(TerminalSize {
                    cols: request.cols,
                    rows: request.rows,
                    pixel_width: request.pixel_width,
                    pixel_height: request.pixel_height,
                    dpi: request.dpi,
                });
                send(&sessions, request.session_id, SessionCommand::Resize(size));
            }
            pack = disconnect_rx.recv() => {
                let Some(pack) = pack else { break };
                send(&sessions, pack.message.session_id, SessionCommand::Disconnect);
            }
            pack = input_rx.recv() => {
                let Some(pack) = pack else { break };
                let session_id = pack.message.session_id;
                if let Some(input) = terminal_input_from_request(pack.message) {
                    send(&sessions, session_id, SessionCommand::Input(input));
                }
            }
            pack = mouse_rx.recv() => {
                let Some(pack) = pack else { break };
                let session_id = pack.message.session_id;
                if let Some(event) = mouse_event_from_request(pack.message) {
                    send(&sessions, session_id, SessionCommand::Mouse(event));
                }
            }
            pack = selection_rx.recv() => {
                let Some(pack) = pack else { break };
                send(&sessions, pack.message.session_id, SessionCommand::Selection(pack.message));
            }
            pack = copy_rx.recv() => {
                let Some(pack) = pack else { break };
                send(&sessions, pack.message.session_id, SessionCommand::Copy);
            }
            pack = paste_rx.recv() => {
                let Some(pack) = pack else { break };
                send(&sessions, pack.message.session_id, SessionCommand::Paste(pack.message.text));
            }
            pack = ack_rx.recv() => {
                let Some(pack) = pack else { break };
                send(&sessions, pack.message.session_id, SessionCommand::FrameAck(pack.message.seq));
            }
            pack = reply_rx.recv() => {
                let Some(pack) = pack else { break };
                if let Some(handle) = sessions.get(&pack.message.session_id) {
                    let _ = handle.replies.send(pack.message);
                }
            }
        }
    }
}

async fn run_session(
    context: Arc<AppContext>,
    mut request: ConnectRequest,
    mut commands: UnboundedReceiver<SessionCommand>,
    mut replies: UnboundedReceiver<InteractionReply>,
) {
    let session_id = request.session_id;
    // 快速连接的密码：所有权直接移进 SecretString（drop 时清零），请求里不留明文副本
    // （PLAN.md §2.2：secret 不可序列化，边界上手写转换）。
    let quick_password = SecretString::from(std::mem::take(&mut request.password));
    // 首个 PTY 尺寸就是连接请求带来的几何（Dart 在布局完成后才发请求）。
    let size = clamped_size(TerminalSize {
        cols: request.cols,
        rows: request.rows,
        pixel_width: request.pixel_width,
        pixel_height: request.pixel_height,
        dpi: request.dpi,
    });

    let target = match connect::resolve_target(&context, &request).await {
        Ok(target) => target,
        Err(abort) => {
            report_abort(session_id, abort, "", 0).await;
            return;
        }
    };
    let described = target.describe();
    let (host, port) = (target.profile.host.clone(), target.profile.port);
    report(session_id, SessionState::Connecting, described.clone());

    // 连接期间收到的输入 / 尺寸变化：连上后先按原顺序处理。
    let mut backlog = VecDeque::new();
    let channels = Channels {
        commands: &mut commands,
        replies: &mut replies,
        backlog: &mut backlog,
    };
    let established =
        connect::establish(&context, session_id, target, quick_password, size, channels).await;
    drop(replies);
    let Connected {
        mut transport,
        profile,
    } = match established {
        Ok(connected) => connected,
        Err(abort) => {
            report_abort(session_id, abort, &host, port).await;
            return;
        }
    };
    debug_print!("[session {session_id}] connected");
    report(session_id, SessionState::Connected, described);

    let terminal = settings::terminal_settings(&context)
        .await
        .resolve(&profile.terminal_overrides);
    let mut engine = match DefaultTerminalEngine::new(&terminal, size) {
        Ok(engine) => engine,
        Err(error) => {
            report_end(
                session_id,
                SessionEnd::failed(FailureKind::Other, format!("engine: {error:?}")),
            );
            let _ = transport.shutdown().await;
            return;
        }
    };
    let mut viewport = Viewport {
        top_stable_row: i64::MAX, // 永远看最底部（活动区），与 bench/探针一致
        rows: size.rows,
    };
    let mut stats = PerfWindow::new();
    let mut pacer = FramePacer::new();
    let mut mouse_motion = MouseMotion::default();
    // 选区权威在引擎（M2a 方案 A）：连接的整个生命周期里持有当前选区，
    // 每次 render 都带上它——内容重排/滚动时高亮跟着引擎走，不是 Dart 侧
    // 自己维护一套坐标。
    let mut selection: Option<SelectionRange> = None;

    let end = 'session: {
        // 连接建立即送第一帧（欢迎横幅可能已经进了引擎）。
        if let Err(detail) = present(
            session_id,
            &mut engine,
            viewport,
            selection,
            &mut stats,
            &mut pacer,
        ) {
            break 'session SessionEnd::failed(FailureKind::Other, detail);
        }
        send_selection_state(session_id, None);

        loop {
            let frame_deadline = pacer.deadline();
            tokio::select! {
                event = transport.next_event() => match event {
                    Ok(TransportEvent::Output(bytes)) => {
                        match engine.advance(&bytes) {
                            Ok(delta) => {
                                // 引擎对远端查询的应答（DA / 光标位置报告…）必须回写，
                                // 否则对端会一直等（m0 探针忽略它只是因为探针不需要应答）。
                                if !delta.outbound.is_empty() {
                                    let _ = transport.write(&delta.outbound).await;
                                }
                                if delta.dirty
                                    && pacer.mark_dirty(Instant::now())
                                    && let Err(detail) =
                                        present(session_id, &mut engine, viewport, selection, &mut stats, &mut pacer)
                                {
                                    break SessionEnd::failed(FailureKind::Other, detail);
                                }
                            }
                            Err(error) => {
                                break SessionEnd::failed(FailureKind::Other, format!("advance: {error:?}"));
                            }
                        }
                    }
                    Ok(TransportEvent::Failure(failure)) => {
                        break SessionEnd::failed(connect::failure_kind(failure.failure()), format!("session: {failure:?}"));
                    }
                    Ok(TransportEvent::Eof) => {
                        break SessionEnd::closed("eof");
                    }
                    Ok(TransportEvent::Exit(status)) => {
                        break SessionEnd::closed(format!(
                            "exit code={:?} success={}",
                            status.code, status.success
                        ));
                    }
                    Ok(_) => {}
                    Err(error) => {
                        break SessionEnd::failed(connect::failure_kind(error.failure()), format!("transport: {error:?}"));
                    }
                },
                // 合帧：高输出期间攒下的内容到点一次性画出。
                () = sleep_until(frame_deadline.unwrap_or_else(Instant::now)), if frame_deadline.is_some() => {
                    if pacer.ready_for_pending(Instant::now())
                        && let Err(detail) =
                            present(session_id, &mut engine, viewport, selection, &mut stats, &mut pacer)
                    {
                        break SessionEnd::failed(FailureKind::Other, detail);
                    }
                }
                command = next_command(&mut backlog, &mut commands) => match command {
                    Some(SessionCommand::Resize(new_size)) => {
                        if let Err(error) = engine.resize(new_size) {
                            break SessionEnd::failed(FailureKind::Other, format!("engine resize: {error:?}"));
                        }
                        viewport.rows = new_size.rows;
                        if let Err(error) = transport.resize(new_size).await {
                            // window-change 失败不立刻判死；远端布局暂旧，后续 resize 可再试。
                            rinf::debug_print!("window-change failed: {error:?}");
                        }
                        if let Err(detail) = present(session_id, &mut engine, viewport, selection, &mut stats, &mut pacer) {
                            break SessionEnd::failed(FailureKind::Other, detail);
                        }
                    }
                    Some(SessionCommand::Input(input)) => {
                        // M2 输入闭环：键编码（ETX/Kitty/CSI-u）在引擎里，
                        // 这里只负责把编码结果写进 transport。
                        match engine.encode_input(input) {
                            Ok(bytes) if !bytes.is_empty() => {
                                if let Err(error) = transport.write(&bytes).await {
                                    break SessionEnd::failed(FailureKind::Other, format!("input write: {error:?}"));
                                }
                            }
                            Ok(_) => {}
                            Err(error) => {
                                break SessionEnd::failed(FailureKind::Other, format!("encode_input: {error:?}"));
                            }
                        }
                    }
                    Some(SessionCommand::Mouse(event)) => {
                        // 远端没开对应的鼠标上报（shell 等）时 encode 返回 Err——
                        // 远端不要这类事件，静默忽略（M2a）。
                        if mouse_motion.admit(&event) {
                            match engine.encode_mouse(event) {
                                Ok(bytes) if !bytes.is_empty() => {
                                    if let Err(error) = transport.write(&bytes).await {
                                        break SessionEnd::failed(FailureKind::Other, format!("mouse write: {error:?}"));
                                    }
                                }
                                Ok(_) | Err(_) => {}
                            }
                        }
                    }
                    Some(SessionCommand::Selection(request)) => {
                        // 选区变化：更新引擎持有的选区 → 重渲染发帧（高亮跟着
                        // 内容走）→ 把引擎的选区原样回显（Dart 用它对耳朵/气泡定位）。
                        selection = selection_range_from_request(request);
                        if let Err(detail) = present(session_id, &mut engine, viewport, selection, &mut stats, &mut pacer) {
                            break SessionEnd::failed(FailureKind::Other, detail);
                        }
                        send_selection_state(session_id, selection);
                    }
                    Some(SessionCommand::Copy) => {
                        // 取文在引擎里（跨行拼接、裁行尾空格都由它负责）。
                        let text = match selection {
                            Some(range) => engine.selected_text(range).unwrap_or_default(),
                            None => String::new(),
                        };
                        ClipboardText { session_id, text }.send_signal_to_dart();
                    }
                    Some(SessionCommand::Paste(text)) => {
                        let bytes = paste_bytes(&text, engine.display_modes().bracketed_paste);
                        if !bytes.is_empty()
                            && let Err(error) = transport.write(&bytes).await
                        {
                            break SessionEnd::failed(FailureKind::Other, format!("paste write: {error:?}"));
                        }
                    }
                    Some(SessionCommand::FrameAck(seq)) => {
                        // 上一帧 Dart 已处理完：攒着的变化现在可以画了（节拍允许的话）。
                        pacer.acked(seq);
                        if pacer.ready_for_pending(Instant::now())
                            && let Err(detail) =
                                present(session_id, &mut engine, viewport, selection, &mut stats, &mut pacer)
                        {
                            break SessionEnd::failed(FailureKind::Other, detail);
                        }
                    }
                    Some(SessionCommand::Disconnect) | None => {
                        break SessionEnd::closed("disconnect");
                    }
                },
            }
        }
    };

    // 会话结束：先把合帧里攒着的变化画出来（最后一屏输出不能丢），再报状态。
    if pacer.has_pending() {
        let _ = present(
            session_id,
            &mut engine,
            viewport,
            selection,
            &mut stats,
            &mut pacer,
        );
    }
    report_end(session_id, end);

    let _ = transport.shutdown().await;
}

/// 下一条命令：连接期间攒下的先出。
async fn next_command(
    backlog: &mut VecDeque<SessionCommand>,
    commands: &mut UnboundedReceiver<SessionCommand>,
) -> Option<SessionCommand> {
    match backlog.pop_front() {
        Some(command) => Some(command),
        None => commands.recv().await,
    }
}

fn report(session_id: u32, state: SessionState, detail: String) {
    SessionStatus {
        session_id,
        state,
        failure: FailureKind::None,
        detail,
        local_network_settings_url: String::new(),
        hint: ConnectHint::None,
    }
    .send_signal_to_dart();
}

fn report_end(session_id: u32, end: SessionEnd) {
    SessionStatus {
        session_id,
        state: end.state,
        failure: end.failure,
        detail: end.detail,
        local_network_settings_url: String::new(),
        hint: ConnectHint::None,
    }
    .send_signal_to_dart();
}

/// 连接没建成：取消报 Closed；失败时顺带判断是否可能是本地网络权限。
async fn report_abort(session_id: u32, abort: Abort, host: &str, port: u16) {
    match abort {
        Abort::Cancelled => report(session_id, SessionState::Cancelled, String::new()),
        Abort::Failed { failure, detail } => SessionStatus {
            session_id,
            state: SessionState::Failed,
            failure,
            detail,
            local_network_settings_url: local_network::settings_url(host, port, failure).await,
            hint: ConnectHint::None,
        }
        .send_signal_to_dart(),
    }
}

/// 鼠标移动只在跨格（或换了按住的键）时上报——与 xterm 一致；指针在同一格内
/// 的移动对远端没有信息量。按下 / 松开也记下位置，紧随其后的同格移动不重报。
#[derive(Default)]
struct MouseMotion {
    last: Option<(u16, u16, Option<MouseButton>)>,
}

impl MouseMotion {
    fn admit(&mut self, event: &TerminalMouseEvent) -> bool {
        let held = match event.kind {
            MouseEventKind::Scroll => return true,
            MouseEventKind::Press | MouseEventKind::Move => event.button,
            MouseEventKind::Release => None,
        };
        let here = Some((event.cell.column, event.viewport_row, held));
        if event.kind == MouseEventKind::Move && here == self.last {
            return false;
        }
        self.last = here;
        true
    }
}

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// 粘贴文本 → 写给远端的字节：
/// * 换行统一成 CR（CRLF / LF → CR，与 xterm / VTE 一致；回车就是 CR）；
/// * 去掉 Tab / 换行以外的 C0 控制字符、DEL 与 C1 控制字符——粘贴内容里的 ESC 等
///   能伪造按键或提前结束括号粘贴（`ESC[201~` 注入），一律不外发；
/// * 远端开了 bracketed paste（DECSET 2004）时包上 `ESC[200~` … `ESC[201~`。
fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let mut body = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                body.push('\r');
            }
            '\n' => body.push('\r'),
            '\t' => body.push('\t'),
            ch if ch.is_control() => {}
            ch => body.push(ch),
        }
    }
    if body.is_empty() {
        return Vec::new();
    }
    if !bracketed {
        return body.into_bytes();
    }
    let mut bytes = Vec::with_capacity(PASTE_START.len() + body.len() + PASTE_END.len());
    bytes.extend_from_slice(PASTE_START);
    bytes.extend_from_slice(body.as_bytes());
    bytes.extend_from_slice(PASTE_END);
    bytes
}

/// 远端 PTY 的尺寸下限：Flutter 在极端布局下可能量出 0 行/列。
fn clamped_size(size: TerminalSize) -> TerminalSize {
    TerminalSize {
        cols: size.cols.max(MIN_COLS),
        rows: size.rows.max(MIN_ROWS),
        ..size
    }
}

/// 渲染当前视口并发帧（附带 5 秒一次的性能汇总）。
fn present<E: TerminalEngine>(
    session_id: u32,
    engine: &mut E,
    viewport: Viewport,
    selection: Option<SelectionRange>,
    stats: &mut PerfWindow,
    pacer: &mut FramePacer,
) -> Result<(), String> {
    pacer.started(Instant::now());
    let render_start = std::time::Instant::now();
    let frame = engine
        .render(viewport, selection)
        .map_err(|error| format!("render: {error:?}"))?;
    let render_us = micros_since(render_start);
    let seq = send_frame(session_id, &frame, stats, render_us);
    pacer.sent(seq, Instant::now());
    if let Some(perf) = stats.maybe_report(session_id) {
        perf.send_signal_to_dart();
    }
    Ok(())
}

/// 帧节拍与流控：
/// * 同一时刻最多一帧在途——Dart 回 [`FrameAck`](crate::signals::FrameAck) 之前不发下一帧，
///   期间的变化只标记待发，Dart 跟不上时只画最新状态（rinf 的队列无界，不能靠它积压）；
/// * 两帧的渲染起点至少相隔 [`Self::INTERVAL`]（上限约 120 Hz），`cat` 大文件这类
///   高输出不会逐块出帧；远小于 60 Hz 源的周期，不会误合并 60 Hz 的刷新；
/// * 空闲后的第一帧立即发（打字回显不等）；ACK 迟迟不来（App 暂停等）时
///   [`Self::ACK_TIMEOUT`] 后照发，防止卡死。
struct FramePacer {
    last_start: Option<Instant>,
    in_flight: Option<(u32, Instant)>,
    pending: bool,
}

impl FramePacer {
    const INTERVAL: Duration = Duration::from_millis(8);
    const ACK_TIMEOUT: Duration = Duration::from_millis(250);

    fn new() -> Self {
        Self {
            last_start: None,
            in_flight: None,
            pending: false,
        }
    }

    /// 内容变了。返回 true = 现在就出帧；false = 已记为待发。
    fn mark_dirty(&mut self, now: Instant) -> bool {
        if self.may_send(now) {
            return true;
        }
        self.pending = true;
        false
    }

    fn has_pending(&self) -> bool {
        self.pending
    }

    /// 有待发的帧，且此刻允许发。
    fn ready_for_pending(&self, now: Instant) -> bool {
        self.pending && self.may_send(now)
    }

    /// 需要醒来检查待发帧的时刻：节拍到点，或在途帧的 ACK 超时。
    /// 等 ACK 的情况下 ACK 本身会唤醒循环，这里只给超时兜底。
    fn deadline(&self) -> Option<Instant> {
        if !self.pending {
            return None;
        }
        let beat = self.last_start.map(|start| start + Self::INTERVAL);
        let ack = self.in_flight.map(|(_, sent)| sent + Self::ACK_TIMEOUT);
        match (beat, ack) {
            (Some(beat), Some(ack)) => Some(beat.max(ack)),
            (beat, ack) => beat.or(ack),
        }
    }

    fn may_send(&self, now: Instant) -> bool {
        let beat_ok = self
            .last_start
            .is_none_or(|start| now >= start + Self::INTERVAL);
        let ack_ok = self
            .in_flight
            .is_none_or(|(_, sent)| now >= sent + Self::ACK_TIMEOUT);
        beat_ok && ack_ok
    }

    /// 开始渲染一帧（节拍从渲染起点算，渲染耗时不推迟下一拍）。
    fn started(&mut self, now: Instant) {
        self.last_start = Some(now);
    }

    /// 帧 `seq` 已发出；它包含此前所有变化。
    fn sent(&mut self, seq: u32, now: Instant) {
        self.in_flight = Some((seq, now));
        self.pending = false;
    }

    /// Dart 处理完了帧 `seq`（以及它之前的帧）。
    fn acked(&mut self, seq: u32) {
        if let Some((in_flight, _)) = self.in_flight
            && seq.wrapping_sub(in_flight) < u32::MAX / 2
        {
            self.in_flight = None;
        }
    }
}

/// 把引擎当前持有的选区回显给 Dart。保留 anchor/focus 的原始角色不排序
/// （`SelectionRange` 渲染/取文时才 `ordered`），拖耳朵越过对端时角色才不会乱。
fn send_selection_state(session_id: u32, selection: Option<SelectionRange>) {
    let state = match selection {
        Some(range) => SelectionState {
            session_id,
            has_selection: true,
            anchor_row: range.start.stable_row,
            anchor_col: range.start.column,
            focus_row: range.end.stable_row,
            focus_col: range.end.column,
        },
        None => SelectionState {
            session_id,
            has_selection: false,
            anchor_row: 0,
            anchor_col: 0,
            focus_row: 0,
            focus_col: 0,
        },
    };
    state.send_signal_to_dart();
}

/// 边界上的选区请求 → 上游 `SelectionRange`。`clear` 或端点相同都归为「无选区」。
fn selection_range_from_request(request: SelectionRequest) -> Option<SelectionRange> {
    if request.clear {
        return None;
    }
    let start = CellPosition {
        stable_row: request.anchor_row,
        column: request.anchor_col,
    };
    let end = CellPosition {
        stable_row: request.focus_row,
        column: request.focus_col,
    };
    if start == end {
        return None;
    }
    Some(SelectionRange {
        start,
        end,
        rectangular: request.rectangular,
    })
}

/// 把边界上的键名解析成上游 `KeyCode`（PLAN §5 M2：编码权威在 Rust）。
/// 返回 `None` = 无法识别的键，静默丢弃（不记日志——键入高频路径）。
fn parse_key_code(key: &str) -> Option<KeyCode> {
    const CHAR_PREFIX: &str = "character:";
    if let Some(rest) = key.strip_prefix(CHAR_PREFIX) {
        let mut chars = rest.chars();
        let first = chars.next()?;
        if chars.next().is_none() {
            return Some(KeyCode::Character(first));
        }
        return None;
    }
    Some(match key {
        "enter" => KeyCode::Enter,
        "escape" => KeyCode::Escape,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page_up" => KeyCode::PageUp,
        "page_down" => KeyCode::PageDown,
        "arrow_up" => KeyCode::ArrowUp,
        "arrow_down" => KeyCode::ArrowDown,
        "arrow_left" => KeyCode::ArrowLeft,
        "arrow_right" => KeyCode::ArrowRight,
        other => {
            let digits = other.strip_prefix('f')?;
            let index = digits.parse::<u8>().ok()?;
            if !(1..=24).contains(&index) {
                return None;
            }
            KeyCode::F(index)
        }
    })
}

fn terminal_input_from_request(request: InputRequest) -> Option<TerminalInput> {
    if !request.text.is_empty() {
        return Some(TerminalInput::CommittedText(request.text));
    }
    let code = parse_key_code(&request.key)?;
    Some(TerminalInput::Key {
        code,
        modifiers: KeyModifiers {
            shift: request.shift,
            control: request.control,
            alt: request.alt,
            super_key: false,
        },
    })
}

/// 鼠标请求 → 上游事件。滚轮必须走 Scroll；press/release 带普通键；
/// move 带按住的键（拖动）或不带（悬停）。
fn mouse_event_from_request(request: MouseRequest) -> Option<TerminalMouseEvent> {
    let kind = match request.kind.as_str() {
        "press" => MouseEventKind::Press,
        "release" => MouseEventKind::Release,
        "move" => MouseEventKind::Move,
        "scroll" => MouseEventKind::Scroll,
        _ => return None,
    };
    let button = match request.button.as_str() {
        "left" => Some(MouseButton::Left),
        "middle" => Some(MouseButton::Middle),
        "right" => Some(MouseButton::Right),
        "wheel_up" => Some(MouseButton::WheelUp),
        "wheel_down" => Some(MouseButton::WheelDown),
        _ => None,
    };
    Some(TerminalMouseEvent {
        kind,
        button,
        cell: CellPosition {
            stable_row: i64::from(request.row),
            column: request.col,
        },
        viewport_row: request.row,
        pixel_x: 0,
        pixel_y: 0,
        modifiers: KeyModifiers {
            shift: request.shift,
            control: request.control,
            alt: request.alt,
            super_key: false,
        },
    })
}

/// 一个统计窗口（5 秒）内的帧开销累计 + 会话帧序号。
struct PerfWindow {
    window_start: std::time::Instant,
    seq: u32,
    frames: u32,
    render_us_total: u64,
    render_us_max: u32,
    pack_us_total: u64,
    pack_us_max: u32,
    bytes_total: u64,
}

impl PerfWindow {
    fn new() -> Self {
        Self {
            window_start: std::time::Instant::now(),
            seq: 0,
            frames: 0,
            render_us_total: 0,
            render_us_max: 0,
            pack_us_total: 0,
            pack_us_max: 0,
            bytes_total: 0,
        }
    }

    fn record(&mut self, render_us: u32, pack_us: u32, bytes: usize) -> u32 {
        self.seq = self.seq.wrapping_add(1);
        self.frames += 1;
        self.render_us_total += u64::from(render_us);
        self.render_us_max = self.render_us_max.max(render_us);
        self.pack_us_total += u64::from(pack_us);
        self.pack_us_max = self.pack_us_max.max(pack_us);
        self.bytes_total += bytes as u64;
        self.seq
    }

    /// 满 5 秒发一条汇总并重开窗口。
    fn maybe_report(&mut self, session_id: u32) -> Option<PerfStats> {
        let elapsed = self.window_start.elapsed();
        if elapsed < std::time::Duration::from_secs(5) || self.frames == 0 {
            return None;
        }
        let frames = self.frames;
        let stats = PerfStats {
            session_id,
            frames,
            window_ms: u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX),
            render_us_avg: (self.render_us_total / u64::from(frames)) as u32,
            render_us_max: self.render_us_max,
            pack_us_avg: (self.pack_us_total / u64::from(frames)) as u32,
            pack_us_max: self.pack_us_max,
            bytes_avg: (self.bytes_total / u64::from(frames)) as u32,
        };
        *self = Self {
            seq: self.seq,
            ..Self::new()
        };
        Some(stats)
    }
}

fn micros_since(start: std::time::Instant) -> u32 {
    u32::try_from(start.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// 发出一帧，返回它的帧序号。
fn send_frame(session_id: u32, frame: &RenderFrame, stats: &mut PerfWindow, render_us: u32) -> u32 {
    let (cursor_col, cursor_row) = match &frame.cursor {
        Some(cursor) => {
            let row = cursor.position.stable_row - frame.viewport_top;
            let visible = row >= 0 && row < i64::try_from(frame.rows.len()).unwrap_or(i64::MAX);
            if visible {
                (i32::from(cursor.position.column), row as i32)
            } else {
                (NO_CURSOR, NO_CURSOR)
            }
        }
        None => (NO_CURSOR, NO_CURSOR),
    };
    let pack_start = std::time::Instant::now();
    let binary = pack_runs(frame);
    let pack_us = micros_since(pack_start);
    let seq = stats.record(render_us, pack_us, binary.len());
    FrameUpdate {
        session_id,
        cols: frame.size.cols,
        rows: frame.size.rows,
        seq,
        cursor_col,
        cursor_row,
        mouse_reporting: frame.mouse_reporting,
        alternate_screen: frame.alternate_screen,
    }
    .send_signal_to_dart(binary);
    seq
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::{
        FramePacer, MouseMotion, mouse_event_from_request, paste_bytes, terminal_input_from_request,
    };
    use crate::signals::{InputRequest, MouseRequest};
    use rshell_m0::rshell_core::{
        KeyCode, MouseButton, MouseEventKind, TerminalInput, TerminalOverrides, TerminalSettingsV1,
        TerminalSize,
    };
    use rshell_m0::rshell_session::{DefaultTerminalEngine, TerminalEngine};
    use std::time::Duration;
    use tokio::time::Instant;

    fn input(key: &str, control: bool) -> Option<TerminalInput> {
        terminal_input_from_request(InputRequest {
            session_id: 1,
            text: String::new(),
            key: key.to_owned(),
            shift: false,
            control,
            alt: false,
        })
    }

    #[test]
    fn named_keys_parse() {
        for (name, expected) in [
            ("enter", KeyCode::Enter),
            ("escape", KeyCode::Escape),
            ("tab", KeyCode::Tab),
            ("backspace", KeyCode::Backspace),
            ("arrow_up", KeyCode::ArrowUp),
            ("arrow_left", KeyCode::ArrowLeft),
        ] {
            assert!(
                matches!(input(name, false), Some(TerminalInput::Key { code, .. }) if code == expected)
            );
        }
        assert!(matches!(
            input("f12", false),
            Some(TerminalInput::Key {
                code: KeyCode::F(12),
                ..
            })
        ));
        assert!(input("f25", false).is_none());
        assert!(input("not_a_key", false).is_none());
    }

    #[test]
    fn function_keys_use_the_dart_wire_format() {
        // Dart 侧（frame_terminal.dart 的 _fKeyNames）发 "f1".."f12"。
        for index in 1..=12u8 {
            let name = format!("f{index}");
            assert!(matches!(
                input(&name, false),
                Some(TerminalInput::Key { code: KeyCode::F(parsed), .. }) if parsed == index
            ));
        }
        assert!(input("f:1", false).is_none());
    }

    #[test]
    fn pacer_sends_first_frame_immediately() {
        let now = Instant::now();
        let mut pacer = FramePacer::new();
        assert!(pacer.mark_dirty(now));
        assert_eq!(pacer.deadline(), None);
    }

    #[test]
    fn pacer_keeps_one_frame_in_flight() {
        let start = Instant::now();
        let mut pacer = FramePacer::new();
        pacer.started(start);
        pacer.sent(1, start);
        // 节拍已过，但上一帧还没 ACK：攒着。
        let later = start + FramePacer::INTERVAL * 2;
        assert!(!pacer.mark_dirty(later));
        assert!(!pacer.ready_for_pending(later));
        // ACK 到了：待发帧可以发。
        pacer.acked(1);
        assert!(pacer.ready_for_pending(later));
    }

    #[test]
    fn pacer_spaces_frames_by_the_interval() {
        let start = Instant::now();
        let mut pacer = FramePacer::new();
        pacer.started(start);
        pacer.sent(1, start);
        pacer.acked(1);
        assert!(!pacer.mark_dirty(start + Duration::from_millis(3)));
        assert_eq!(pacer.deadline(), Some(start + FramePacer::INTERVAL));
        // 60 Hz 的源（16.7 ms 一次）不会被合并。
        assert!(pacer.ready_for_pending(start + Duration::from_micros(16_700)));
    }

    #[test]
    fn pacer_gives_up_on_a_missing_ack() {
        let start = Instant::now();
        let mut pacer = FramePacer::new();
        pacer.started(start);
        pacer.sent(7, start);
        assert!(!pacer.mark_dirty(start + FramePacer::INTERVAL));
        assert_eq!(pacer.deadline(), Some(start + FramePacer::ACK_TIMEOUT));
        assert!(pacer.ready_for_pending(start + FramePacer::ACK_TIMEOUT));
    }

    #[test]
    fn pacer_ignores_stale_acks() {
        let start = Instant::now();
        let mut pacer = FramePacer::new();
        pacer.started(start);
        pacer.sent(5, start);
        pacer.acked(4);
        assert!(!pacer.mark_dirty(start + FramePacer::INTERVAL));
        pacer.acked(5);
        assert!(pacer.ready_for_pending(start + FramePacer::INTERVAL));
    }

    #[test]
    fn paste_normalizes_newlines_to_carriage_returns() {
        assert_eq!(paste_bytes("a\nb\r\nc\rd", false), b"a\rb\rc\rd");
    }

    #[test]
    fn paste_is_bracketed_only_when_the_remote_asked() {
        assert_eq!(paste_bytes("ls\n", false), b"ls\r");
        assert_eq!(paste_bytes("ls\n", true), b"\x1b[200~ls\r\x1b[201~");
    }

    #[test]
    fn paste_drops_control_characters_that_could_escape_the_bracket() {
        // 内嵌的 ESC[201~ 会提前结束括号粘贴，后面的文本就被当成键入执行。
        assert_eq!(
            paste_bytes("safe\x1b[201~rm -rf ~\n", true),
            b"\x1b[200~safe[201~rm -rf ~\r\x1b[201~"
        );
        assert_eq!(paste_bytes("tab\there\u{7f}\u{9b}x", false), b"tab\therex");
        assert_eq!(paste_bytes("\x03", true), b"");
    }

    #[test]
    fn character_key_parses() {
        assert!(matches!(
            input("character:c", false),
            Some(TerminalInput::Key {
                code: KeyCode::Character('c'),
                ..
            })
        ));
        // 多字符不是合法键
        assert!(input("character:ab", false).is_none());
    }

    #[test]
    fn text_wins_and_carries_no_key() {
        let request = InputRequest {
            session_id: 1,
            text: "你好".to_owned(),
            key: String::new(),
            shift: false,
            control: false,
            alt: false,
        };
        assert!(matches!(
            terminal_input_from_request(request),
            Some(TerminalInput::CommittedText(text)) if text == "你好"
        ));
    }

    #[test]
    fn modifiers_survive_the_boundary() {
        assert!(matches!(
            input("character:c", true),
            Some(TerminalInput::Key { code: KeyCode::Character('c'), modifiers })
                if modifiers.control && !modifiers.shift && !modifiers.alt
        ));
    }

    fn mouse(kind: &str, button: &str, col: u16, row: u16) -> MouseRequest {
        MouseRequest {
            session_id: 1,
            kind: kind.to_owned(),
            button: button.to_owned(),
            col,
            row,
            shift: false,
            control: false,
            alt: false,
        }
    }

    #[test]
    fn mouse_requests_parse() {
        let drag = mouse_event_from_request(mouse("move", "left", 3, 2)).expect("drag");
        assert_eq!(drag.kind, MouseEventKind::Move);
        assert_eq!(drag.button, Some(MouseButton::Left));
        assert_eq!((drag.cell.column, drag.viewport_row), (3, 2));

        let hover = mouse_event_from_request(mouse("move", "", 0, 0)).expect("hover");
        assert_eq!(hover.button, None);

        let control = mouse_event_from_request(MouseRequest {
            control: true,
            ..mouse("press", "right", 1, 1)
        })
        .expect("press");
        assert!(control.modifiers.control && !control.modifiers.shift);

        assert!(mouse_event_from_request(mouse("hover", "", 0, 0)).is_none());
    }

    #[test]
    fn mouse_moves_are_reported_once_per_cell() {
        let mut motion = MouseMotion::default();
        let mut admit = |kind, button, col, row| {
            motion.admit(&mouse_event_from_request(mouse(kind, button, col, row)).expect("event"))
        };

        assert!(admit("press", "left", 1, 1));
        assert!(!admit("move", "left", 1, 1), "same cell as the press");
        assert!(admit("move", "left", 2, 1));
        assert!(!admit("move", "left", 2, 1));
        assert!(admit("release", "left", 2, 1));
        assert!(
            !admit("move", "", 2, 1),
            "hover right where the button was released"
        );
        assert!(admit("move", "", 3, 1));
        assert!(
            admit("move", "right", 3, 1),
            "same cell, different button held"
        );
        assert!(admit("scroll", "wheel_up", 3, 1));
        assert!(admit("scroll", "wheel_up", 3, 1), "every wheel step counts");
    }

    /// 拖动与悬停按远端的鼠标模式编码：1000 只要点击，1002 加拖动，1003 加悬停。
    #[test]
    fn mouse_motion_is_encoded_per_tracking_mode() {
        let profile = TerminalSettingsV1::default().resolve(&TerminalOverrides::default());
        let size = TerminalSize {
            cols: 80,
            rows: 24,
            pixel_width: 0,
            pixel_height: 0,
            dpi: 0,
        };
        let mut engine = DefaultTerminalEngine::new(&profile, size).expect("engine");
        let encode = |engine: &mut DefaultTerminalEngine, kind, button| {
            let event = mouse_event_from_request(mouse(kind, button, 3, 2)).expect("event");
            engine.encode_mouse(event).ok()
        };

        engine
            .advance(b"\x1b[?1000h\x1b[?1006h")
            .expect("click tracking");
        assert_eq!(
            encode(&mut engine, "press", "left"),
            Some(b"\x1b[<0;4;3M".to_vec())
        );
        assert_eq!(encode(&mut engine, "move", "left"), None);

        engine.advance(b"\x1b[?1002h").expect("drag tracking");
        assert_eq!(
            encode(&mut engine, "move", "left"),
            Some(b"\x1b[<32;4;3M".to_vec())
        );
        assert_eq!(encode(&mut engine, "move", ""), None);

        engine.advance(b"\x1b[?1003h").expect("any-motion tracking");
        assert_eq!(
            encode(&mut engine, "move", ""),
            Some(b"\x1b[<35;4;3M".to_vec())
        );
        assert_eq!(
            encode(&mut engine, "release", "left"),
            Some(b"\x1b[<0;4;3m".to_vec())
        );
    }
}
