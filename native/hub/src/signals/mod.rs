//! rinf 边界上的信号类型（适配层）。
//!
//! 业务模型（`RenderFrame` / `ConnectionProfile` ...）属于上游 `rshell-core`，
//! 不在这里重建（PLAN.md 铁律 1/4）。这里只放边界上的平铺投影：
//! * 本文件：启动与会话（请求、状态、帧——帧的 run 压缩字节走二进制通道）
//! * [`catalog`]：连接目录；[`keys`]：私钥；[`interaction`]：连接过程中的交互；
//!   [`settings`]：设置
//!
//! 会话类信号都带 `session_id`（Dart 分配，本进程内唯一）：rinf 的信号按类型
//! 全局广播，多个会话并存时靠它分流。

pub mod catalog;
pub mod interaction;
pub mod keys;
pub mod settings;

use rinf::{DartSignal, RustSignal, RustSignalBinary, SignalPiece};
use serde::{Deserialize, Serialize};

// ── 启动 ─────────────────────────────────────────────────────────────────────

/// App 启动时发一次：平台给 App 的私有数据目录（Flutter 用 path_provider 取）。
/// 连接目录（SQLite）与 known_hosts 都放在这里。
#[derive(Deserialize, DartSignal)]
pub struct AppStart {
    pub support_dir: String,
}

/// 启动结果。`ok = false` 时存储不可用（`detail` 说明原因），目录与连接都不能用。
#[derive(Serialize, RustSignal)]
pub struct AppReady {
    pub ok: bool,
    pub detail: String,
}

// ── Dart → Rust ──────────────────────────────────────────────────────────────

/// 建立一条 SSH 会话：`connection_id` 非空连目录里的连接；为空是**快速连接**
/// （不存目录），目标取下面几个字段。
///
/// 快速连接的密码可以随请求带来（按明文过边界——上游的 `SecretString` 刻意不可
/// 序列化，PLAN.md §2.2；Rust 在接收处立刻包成 `SecretString`），为空则连接时弹框问。
#[derive(Deserialize, DartSignal)]
pub struct ConnectRequest {
    pub session_id: u32,
    pub connection_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// 非空 = exec 模式：连接后在远端直接执行该命令（如 `top`），不进 shell。
    pub command: String,
    /// 首次几何。度量的唯一权威是 Flutter（PLAN.md §8）：
    /// 它量完格子后随连接请求一起带来。
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub dpi: u32,
}

/// 视口变化（旋转 / 改窗口）。→ `engine.resize` + `transport.resize`（远端 window-change）。
#[derive(Deserialize, DartSignal)]
pub struct ResizeRequest {
    pub session_id: u32,
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub dpi: u32,
}

#[derive(Deserialize, DartSignal)]
pub struct DisconnectRequest {
    pub session_id: u32,
}

/// 终端输入（M2 输入闭环的边界）。
///
/// 键与文本二选一：`text` 非空 = IME 提交/粘贴的文本（`CommittedText`）；
/// 否则 `key` 携带键名——`"character:x"`（单字符）或命名键
/// （enter/escape/tab/backspace/delete/insert/home/end/page_up/page_down/
/// arrow_up/arrow_down/arrow_left/arrow_right/`f1`…`f24`）。
/// 键编码（ETX/Kitty/CSI-u…）是 Rust 侧 `encode_input` 的事，Dart 只转发。
#[derive(Deserialize, DartSignal)]
pub struct InputRequest {
    pub session_id: u32,
    pub text: String,
    pub key: String,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

/// 鼠标事件（M2a 鼠标转发的边界）。
///
/// Rust 组装成 `TerminalMouseEvent` 交给 `engine.encode_mouse`；远端没开
/// 对应的鼠标上报（点击 / 拖动 / 任意移动）时 encode 返回 Err，静默忽略。
/// 滚轮必须用 `kind: "scroll"`（上游 validate 会拒绝「滚轮走 press」）。
#[derive(Deserialize, DartSignal)]
pub struct MouseRequest {
    pub session_id: u32,
    /// press / release / move / scroll
    pub kind: String,
    /// left / middle / right / wheel_up / wheel_down；无键移动（悬停）为空
    pub button: String,
    pub col: u16,
    pub row: u16,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

/// 选区变更（M2a 方案 A：选区的权威在引擎，M2a 之前我们在 Dart 侧重造了一遍）。
///
/// `clear = true` 表示清除，其余字段忽略；否则 anchor/focus 是**引擎绝对行号**
/// （`stable_row`，不是视口行号——Dart 已换算好）。两个端点不分先后，引擎
/// 渲染/取文时自己排序，所以拖耳朵越过对端不用特殊处理。
#[derive(Deserialize, DartSignal)]
pub struct SelectionRequest {
    pub session_id: u32,
    pub clear: bool,
    pub anchor_row: i64,
    pub anchor_col: u16,
    pub focus_row: i64,
    pub focus_col: u16,
    /// 方块选（列选区）。M2a 只做整行，恒 false。
    pub rectangular: bool,
}

/// 复制当前选区（取文在引擎里，见 `TerminalEngine::selected_text`）。
#[derive(Deserialize, DartSignal)]
pub struct CopyRequest {
    pub session_id: u32,
}

/// 粘贴一段文本。换行规范化、控制字符过滤、按远端模式包 bracketed paste
/// 都是 Rust 的事（`session::paste_bytes`），Dart 只转发剪贴板原文。
#[derive(Deserialize, DartSignal)]
pub struct PasteRequest {
    pub session_id: u32,
    pub text: String,
}

/// Dart 已处理完 `seq` 这一帧（流控：同一时刻最多一帧在途，Dart 跟不上时
/// Rust 只保留最新状态，不在 rinf 的无界队列里积压）。
#[derive(Deserialize, DartSignal)]
pub struct FrameAck {
    pub session_id: u32,
    pub seq: u32,
}

// ── Rust → Dart ──────────────────────────────────────────────────────────────

#[derive(Serialize, SignalPiece)]
pub enum SessionState {
    Connecting,
    Connected,
    Failed,
    /// 会话结束（远端退出、断开）。
    Closed,
    /// 连接完成前被用户取消（关闭页面、取消密码框）。
    Cancelled,
}

/// 失败分类（`SessionState::Failed` 时有意义）。文案由 Dart 按分类给出。
#[derive(Serialize, SignalPiece, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    None,
    /// 目录里没有这条连接（已被删除）。
    NotFound,
    /// 快速连接的目标无效（主机、端口或用户名）。
    InvalidTarget,
    /// 连接用的私钥不在钥匙串里了。
    KeyNotFound,
    Authentication,
    HostKeyRejected,
    /// 主机密钥与记录的不一致，且没有被接受替换。
    HostKeyChanged,
    Network,
    Timeout,
    /// 钥匙串读写失败。
    Keychain,
    Other,
}

#[derive(Serialize, RustSignal)]
pub struct SessionStatus {
    pub session_id: u32,
    pub state: SessionState,
    pub failure: FailureKind,
    /// 补充信息（目标地址、诊断分类等）。绝不包含密码。
    pub detail: String,
    /// 失败可能源于系统的本地网络权限（目标在局域网，失败是网络或超时类）。
    /// 非空时是打开系统设置对应页面的 URL（平台相关，由 Rust 给出）。
    pub local_network_settings_url: String,
}

/// 一帧终端画面。二进制部分是 [`crate::frame_codec::pack_runs`] 的产物，
/// 走 `RustSignalBinary` 的原始字节通道（PLAN.md §4.3 的落地方案）。
#[derive(Serialize, RustSignalBinary)]
pub struct FrameUpdate {
    pub session_id: u32,
    pub cols: u16,
    pub rows: u16,
    /// 本会话内单调递增的帧序号。Dart 侧用它数**丢帧**：
    /// 收到的 seq 跳变 = 中间有帧没送达（验收：表现为晚一帧，不是花屏）。
    pub seq: u32,
    /// 光标的视口内坐标（列, 行）。`-1` 表示不可见（隐藏或滚出视口）。
    pub cursor_col: i32,
    pub cursor_row: i32,
    /// 远端开启了鼠标上报（DECSET 1000/1002/1003）。Dart 据此决定
    /// 触摸点击/滚轮是转发给远端还是保持本地行为（M2a）。
    pub mouse_reporting: bool,
    /// 远端在备用屏（vim/less 等 TUI）。
    pub alternate_screen: bool,
}

/// 选区回显（引擎当前持有选区的原样投影）。
///
/// Dart 用它给耳朵/气泡定位。`anchor`/`focus` **保留 Dart 传入时的角色、不排序**
/// ——拖耳朵越过对端时角色才不会乱（引擎自己渲染/取文时才排序）。
#[derive(Serialize, RustSignal)]
pub struct SelectionState {
    pub session_id: u32,
    pub has_selection: bool,
    pub anchor_row: i64,
    pub anchor_col: u16,
    pub focus_row: i64,
    pub focus_col: u16,
}

/// 引擎取出的选区文本。引擎已按自己的规则跨行拼接、裁掉行尾空格（`text.rs`
/// 的 `selection_text`），Dart 直接进剪贴板。无选区时是空串。
#[derive(Serialize, RustSignal)]
pub struct ClipboardText {
    pub session_id: u32,
    pub text: String,
}

/// 每 5 秒一条的性能汇总（M1 帧率验收的数字来源）。
/// 单帧预算 16.67 ms（PLAN §4）：render_us + pack_us 的 max 是 Rust 侧的真实开销。
#[derive(Serialize, RustSignal)]
pub struct PerfStats {
    pub session_id: u32,
    /// 统计窗口内实际打包发出的帧数。fps = frames / window_ms * 1000。
    pub frames: u32,
    pub window_ms: u32,
    pub render_us_avg: u32,
    pub render_us_max: u32,
    pub pack_us_avg: u32,
    pub pack_us_max: u32,
    /// 每帧压缩字节数的平均值（典型 5.8–7.2 KB，TUI 满屏最坏 61 KB）。
    pub bytes_avg: u32,
}
