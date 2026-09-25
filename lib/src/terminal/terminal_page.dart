import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';
import 'package:rinf/rinf.dart';
import 'package:terminal_view/terminal_view.dart'
    show
        CellOffset,
        defaultTerminalShortcuts,
        PointerInputs,
        TerminalController,
        TerminalStyle,
        TerminalView,
        TerminalThemes,
        TerminalMouseButton;
// 字符度量是 fork 的内部工具，但选区菜单的锚点定位要用它
// （与行身份缓冲同一类实现级依赖）。
// ignore: implementation_imports
import 'package:terminal_view/src/ui/char_metrics.dart';
import 'package:url_launcher/url_launcher.dart';

import 'package:guosh_shell/src/bindings/bindings.dart';

import '../settings/terminal_font.dart';
import 'frame.dart';
import 'frame_terminal.dart';
import 'prompt_dialogs.dart';
import 'session_target.dart';
import 'terminal_key_bar.dart';

/// 本进程内会话编号（rinf 信号按类型全局广播，靠它分流）。
int _nextSessionId = 1;

/// 复制 / 全选的快捷键（⌘C / ⌘A 等，按平台由 fork 的默认表给出）改走引擎：
/// 选区权威在引擎（PLAN §6.1 M2a 选区决定），fork 自带的这两个动作用的是
/// Dart 侧缓冲，拿不到滚出视口的内容，全选也不会告诉引擎。
class _EngineCopyIntent extends Intent {
  const _EngineCopyIntent();
}

class _EngineSelectAllIntent extends Intent {
  const _EngineSelectAllIntent();
}

final Map<ShortcutActivator, Intent> _terminalShortcuts = {
  for (final entry in defaultTerminalShortcuts.entries)
    entry.key: switch (entry.value) {
      CopySelectionTextIntent() => const _EngineCopyIntent(),
      SelectAllTextIntent() => const _EngineSelectAllIntent(),
      final other => other,
    },
};

/// 一次 SSH 会话的页面：终端（fork 渲染 + 帧驱动适配器 + 键位条）。
/// 会话在页面打开后、终端完成首次布局时发起（几何随连接请求一起带上）。
class TerminalPage extends StatefulWidget {
  final SessionTarget target;

  const TerminalPage({super.key, required this.target});

  @override
  State<TerminalPage> createState() => _TerminalPageState();
}

class _TerminalPageState extends State<TerminalPage> {
  final FrameTerminal _terminal = FrameTerminal();
  final TerminalController _terminalController =
      TerminalController(pointerInputs: const PointerInputs.all());
  /// 软键盘的开关靠它：焦点在终端上 = 键盘起，unfocus = 收起。
  final FocusNode _terminalFocus = FocusNode();
  /// 选区菜单锚点定位用（终端渲染区的屏幕坐标）。
  final GlobalKey _terminalSurfaceKey = GlobalKey();
  /// Flutter 自带的选区菜单（iOS 上是系统风格气垫）。
  final ContextMenuController _selectionMenu = ContextMenuController();

  /// 终端样式：视图渲染与选区菜单锚点定位共用同一份（取自设置，页面内不变）。
  late final TerminalStyle _style = terminalStyle(SettingsState.latestRustSignal!.message);

  StreamSubscription? _statusSub;
  StreamSubscription? _frameSub;
  StreamSubscription? _selectionSub;
  StreamSubscription? _clipboardSub;
  StreamSubscription? _perfSub;
  StreamSubscription? _promptSub;

  /// 当前会话编号；重连换新编号，旧会话迟到的信号不再生效。
  int _sessionId = 0;

  /// 会话已失败 / 结束：还开着的交互对话框据此自行关闭。
  final ValueNotifier<bool> _promptsDismissed = ValueNotifier(false);

  /// 引擎最近一次选区回显（绝对行坐标）。视口一变就要拿它重新投影。
  SelectionState? _selectionEcho;
  /// 上次投影时的视口映射（首/末绝对行）。变了才重新投影——
  /// 拖动中的乐观更新不会被迟到的回显顶掉。
  int? _projectedFirstStable;
  int? _projectedLastStable;

  SessionState? _state;
  FailureKind _failure = FailureKind.none;
  String _detail = '';
  String _localNetworkSettingsUrl = '';

  @override
  void initState() {
    super.initState();
    _statusSub = SessionStatus.rustSignalStream.listen(_onStatus);
    _frameSub = FrameUpdate.rustSignalStream.listen(_onFrame);
    _selectionSub = SelectionState.rustSignalStream.listen(_onSelectionState);
    _clipboardSub = ClipboardText.rustSignalStream.listen(_onClipboardText);
    _promptSub = InteractionPrompt.rustSignalStream.listen(_onPrompt);
    if (kDebugMode) {
      _perfSub = PerfStats.rustSignalStream.listen(_onPerfStats);
    }
    _terminal
      ..onInput = _onTerminalInput
      ..onResize = _onTerminalResize;
    _terminalController
      ..addListener(_onSelectionChanged)
      ..onSelectionIntent = _onSelectionIntent;
    _startSession();
  }

  @override
  void dispose() {
    // 任何方式离开页面都结束会话（连接中的也一并取消）。
    if (_sessionId != 0 && !_ended) DisconnectRequest(sessionId: _sessionId).sendSignalToRust();
    _statusSub?.cancel();
    _frameSub?.cancel();
    _selectionSub?.cancel();
    _clipboardSub?.cancel();
    _perfSub?.cancel();
    _promptSub?.cancel();
    _promptsDismissed.dispose();
    _terminalController
      ..removeListener(_onSelectionChanged)
      ..onSelectionIntent = null;
    _selectionMenu.remove();
    _resizeTimer?.cancel();
    _terminalController.dispose();
    _terminalFocus.dispose();
    super.dispose();
  }

  bool get _ended =>
      _state == SessionState.failed ||
      _state == SessionState.closed ||
      _state == SessionState.cancelled;

  void _onStatus(RustSignalPack<SessionStatus> pack) {
    final msg = pack.message;
    if (!mounted || msg.sessionId != _sessionId) return;
    final state = msg.state;
    // 连接前被取消（关了密码框等）：回到上一页。
    if (state == SessionState.cancelled) {
      _state = state;
      Navigator.of(context).maybePop();
      return;
    }
    // 会话结束（失败/断开）：引擎那边选区没了，别再留着高亮/耳朵；
    // 还开着的交互对话框也收起。
    if (state == SessionState.closed || state == SessionState.failed) {
      _selectionEcho = null;
      _projectedFirstStable = null;
      _projectedLastStable = null;
      _terminalController.setExternalSelection(null, null);
      _promptsDismissed.value = true;
    }
    setState(() {
      _state = state;
      _failure = msg.failure;
      _detail = msg.detail;
      _localNetworkSettingsUrl = msg.localNetworkSettingsUrl;
    });
    // 连接中视图几何可能又变了（键盘弹出、旋转）：连上即补发最新尺寸。
    if (state == SessionState.connected) _flushResize();
  }

  /// 连接过程中的问题（密码、主机密钥、keyboard-interactive）→ 对话框 → 回答。
  Future<void> _onPrompt(RustSignalPack<InteractionPrompt> pack) async {
    final prompt = pack.message;
    if (!mounted || prompt.sessionId != _sessionId) return;
    final answer = await showPromptDialog(context, prompt, _promptsDismissed);
    if (prompt.sessionId != _sessionId) return;
    InteractionReply(
      sessionId: prompt.sessionId,
      promptId: prompt.promptId,
      accept: answer.accept,
      answers: answer.answers,
      remember: answer.remember,
    ).sendSignalToRust();
  }

  void _onFrame(RustSignalPack<FrameUpdate> pack) {
    final msg = pack.message;
    if (!mounted || msg.sessionId != _sessionId) return;
    try {
      final frame = decodeFrame(
        pack.binary,
        cols: msg.cols,
        rows: msg.rows,
        cursorCol: msg.cursorCol,
        cursorRow: msg.cursorRow,
      );
      // 显示模式随帧走（RenderFrame 已带，M2a 起过边界）：
      // 决定触摸点击/滚轮是转发远端还是保持本地行为。
      _terminal
        ..mouseReporting = msg.mouseReporting
        ..alternateScreen = msg.alternateScreen;
      _terminal.applyFrame(frame);
      // 滚动/重排会改 stable→视口 的映射；映射变了就重新投影选区，
      // 高亮和耳朵才会跟着内容走（拖动进行中映射不变，不会被顶掉）。
      final first = _terminal.stableRowAt(0);
      final last =
          _terminal.height > 0 ? _terminal.stableRowAt(_terminal.height - 1) : null;
      if (first != _projectedFirstStable || last != _projectedLastStable) {
        _projectedFirstStable = first;
        _projectedLastStable = last;
        _projectSelection();
      }
      // 重排可能让选区端点失效：这时把还挂着的菜单收回，别留个孤儿气泡。
      if (_selectionMenu.isShown && _terminalController.selection == null) {
        _selectionMenu.remove();
      }
    } catch (error) {
      debugPrint('[frame] decode failed: $error');
    } finally {
      // 流控：Rust 等到这一帧的 ACK 才发下一帧（解码失败也要回，免得它空等）。
      FrameAck(sessionId: msg.sessionId, seq: msg.seq).sendSignalToRust();
    }
  }

  /// debug 构建的帧率自检（只打日志、不画浮层）：Rust 每 5 秒汇总一次
  /// 出帧数与渲染/打包耗时。
  void _onPerfStats(RustSignalPack<PerfStats> pack) {
    final p = pack.message;
    if (p.sessionId != _sessionId) return;
    final fps = p.windowMs == 0 ? 0 : p.frames * 1000 / p.windowMs;
    debugPrint('[perf] ${fps.toStringAsFixed(1)} fps · render avg ${p.renderUsAvg}µs '
        'max ${p.renderUsMax}µs · pack avg ${p.packUsAvg}µs · ${p.bytesAvg} B/frame');
  }

  /// 引擎回显选区 → 记住（绝对行坐标）→ 投影到当前视口。
  void _onSelectionState(RustSignalPack<SelectionState> pack) {
    if (!mounted || pack.message.sessionId != _sessionId) return;
    _selectionEcho = pack.message;
    _projectSelection();
  }

  /// 把引擎回显的选区（绝对行）投影到当前视口并喂给 fork。
  /// 端点滚出视口时贴边截断（可见部分保留高亮），整体不可见才清空。
  void _projectSelection() {
    final echo = _selectionEcho;
    if (echo == null || !echo.hasSelection) {
      _terminalController.setExternalSelection(null, null);
      return;
    }
    final rows = _terminal.height;
    final firstStable = rows > 0 ? _terminal.stableRowAt(0) : null;
    final lastStable = rows > 0 ? _terminal.stableRowAt(rows - 1) : null;
    if (firstStable == null || lastStable == null) {
      _terminalController.setExternalSelection(null, null);
      return;
    }

    // 排序出首端/尾端（首端 = 早的那个），角色保留给 begin/end。
    final anchorIsFirst = echo.anchorRow < echo.focusRow ||
        (echo.anchorRow == echo.focusRow && echo.anchorCol <= echo.focusCol);
    final firstStableRow = anchorIsFirst ? echo.anchorRow : echo.focusRow;
    final lastStableRow = anchorIsFirst ? echo.focusRow : echo.anchorRow;
    final firstCol = anchorIsFirst ? echo.anchorCol : echo.focusCol;
    final lastCol = anchorIsFirst ? echo.focusCol : echo.anchorCol;

    // 首端滚到视口下方 / 尾端滚到视口上方 = 整个选区都看不见。
    if (firstStableRow > lastStable || lastStableRow < firstStable) {
      _terminalController.setExternalSelection(null, null);
      return;
    }

    final firstRow = _terminal.viewportRowForStable(firstStableRow);
    final firstOffset = firstRow != null
        ? CellOffset(firstCol, firstRow)
        : const CellOffset(0, 0); // 上方滚出：贴到首行首格
    final lastRow = _terminal.viewportRowForStable(lastStableRow);
    final lastOffset = lastRow != null
        ? CellOffset(lastCol, lastRow)
        : CellOffset(0, rows); // 下方滚出：贴到末行之后（排他端）

    _terminalController.setExternalSelection(
      anchorIsFirst ? firstOffset : lastOffset,
      anchorIsFirst ? lastOffset : firstOffset,
    );
  }

  /// 引擎取文回来 → 进剪贴板 + 清选区（对齐 Termux）。
  void _onClipboardText(RustSignalPack<ClipboardText> pack) {
    if (!mounted || pack.message.sessionId != _sessionId) return;
    Clipboard.setData(ClipboardData(text: pack.message.text));
    _sendSelectionRequest(clear: true);
  }

  /// fork 上报的选区意图 → 换算成引擎绝对行 → 发 SelectionRequest。
  void _onSelectionIntent(CellOffset? begin, CellOffset? end) {
    if (_state != SessionState.connected) return;
    if (begin == null || end == null) {
      _sendSelectionRequest(clear: true);
      return;
    }
    final anchorRow = _terminal.stableRowAt(begin.y);
    final focusRow = _terminal.stableRowAt(end.y);
    if (anchorRow == null || focusRow == null) return;
    SelectionRequest(
      sessionId: _sessionId,
      clear: false,
      anchorRow: anchorRow,
      anchorCol: begin.x,
      focusRow: focusRow,
      focusCol: end.x,
      rectangular: false,
    ).sendSignalToRust();
  }

  void _sendSelectionRequest({required bool clear}) {
    SelectionRequest(
      sessionId: _sessionId,
      clear: clear,
      anchorRow: 0,
      anchorCol: 0,
      focusRow: 0,
      focusCol: 0,
      rectangular: false,
    ).sendSignalToRust();
  }

  /// fork 的输入口 → rinf → Rust（键编码权威在 encode_input）。
  /// 连接中的输入也照发：Rust 连上后按顺序补上（type-ahead）。
  bool _onTerminalInput(TerminalInputEvent event) {
    if (_state != SessionState.connected && _state != SessionState.connecting) return false;
    switch (event) {
      case KeyInputEvent(:final key, :final shift, :final control, :final alt):
        InputRequest(
          sessionId: _sessionId,
          text: '',
          key: key,
          shift: shift,
          control: control,
          alt: alt,
        ).sendSignalToRust();
      case TextInputEvent(:final text):
        InputRequest(
          sessionId: _sessionId,
          text: text,
          key: '',
          shift: false,
          control: false,
          alt: false,
        ).sendSignalToRust();
      case PasteInputEvent(:final text):
        PasteRequest(sessionId: _sessionId, text: text).sendSignalToRust();
      case MouseInputEvent(
          :final button,
          :final action,
          :final position,
          :final shift,
          :final alt,
          :final ctrl,
        ):
        // 滚轮走 Scroll（上游 validate 拒绝「滚轮走 press」）。
        MouseRequest(
          sessionId: _sessionId,
          kind: button != null && button.isWheel
              ? 'scroll'
              : switch (action) {
                  MouseAction.press => 'press',
                  MouseAction.release => 'release',
                  MouseAction.move => 'move',
                },
          button: switch (button) {
            TerminalMouseButton.left => 'left',
            TerminalMouseButton.middle => 'middle',
            TerminalMouseButton.right => 'right',
            TerminalMouseButton.wheelUp => 'wheel_up',
            TerminalMouseButton.wheelDown => 'wheel_down',
            TerminalMouseButton.wheelLeft || TerminalMouseButton.wheelRight || null => '',
          },
          col: position.x,
          row: position.y,
          shift: shift,
          control: ctrl,
          alt: alt,
        ).sendSignalToRust();
    }
    return true;
  }

  /// fork 的 render 在布局期报几何（只在变化时）→ Rust（度量的权威在 Flutter）。
  /// 有待发的连接请求时带着真实几何发 ConnectRequest——否则远端 PTY 只能按
  /// 缺省 2×2 建立，exec 输出会在换行历史里塞满垃圾。
  void _onTerminalResize(TerminalGeometry geometry) {
    if (_connectPending) {
      _flushPendingConnect();
      return;
    }
    // 连接中的几何变化先不发：连上时由 _onStatus 补发（见 _flushResize）。
    if (_state != SessionState.connected || geometry == _sentGeometry) return;
    // 软键盘/旋转动画期间 fork 会**逐帧**报新几何；每一步都转发的话，
    // 远端 shell 会为每一步重画一次提示符（实机实测：一次键盘弹出刷了
    // 8 行提示符）。去抖后只发最终尺寸。
    _resizeTimer?.cancel();
    _resizeTimer = Timer(const Duration(milliseconds: 150), _flushResize);
  }

  /// 把最新几何发给会话（与已发的相同则不发）。
  void _flushResize() {
    _resizeTimer?.cancel();
    _resizeTimer = null;
    if (!mounted || _state != SessionState.connected) return;
    final geometry = _terminal.measuredGeometry;
    if (geometry == null || geometry == _sentGeometry) return;
    _sentGeometry = geometry;
    ResizeRequest(
      sessionId: _sessionId,
      cols: geometry.cols,
      rows: geometry.rows,
      pixelWidth: geometry.pixelWidth,
      pixelHeight: geometry.pixelHeight,
      dpi: _dpi,
    ).sendSignalToRust();
  }

  /// 有待发的连接请求且已量到几何 → 发出 ConnectRequest。
  /// 还没有几何（首次布局前）就等 fork 的 resize 回调再发。
  void _flushPendingConnect() {
    final geometry = _terminal.measuredGeometry;
    if (!_connectPending || geometry == null || !mounted) return;
    _connectPending = false;
    _sentGeometry = geometry;
    final target = widget.target;
    ConnectRequest(
      sessionId: _sessionId,
      connectionId: target.connectionId,
      host: target.host,
      port: target.port,
      username: target.username,
      password: target.password,
      command: target.command,
      cols: geometry.cols,
      rows: geometry.rows,
      pixelWidth: geometry.pixelWidth,
      pixelHeight: geometry.pixelHeight,
      dpi: _dpi,
    ).sendSignalToRust();
  }

  int get _dpi => (96 * MediaQuery.devicePixelRatioOf(context)).round();

  /// 已随 ConnectRequest / ResizeRequest 发给当前会话的几何。
  TerminalGeometry? _sentGeometry;
  Timer? _resizeTimer;

  /// 软键盘开关（键位条「⌨」键）：焦点在终端 = 键盘起，否则收起。
  void _toggleKeyboard() {
    if (_terminalFocus.hasFocus) {
      _terminalFocus.unfocus();
    } else {
      _terminalFocus.requestFocus();
    }
  }

  /// 选区变化 → 系统风格的选区菜单（Flutter 自带，iOS 上渲染成气垫）。
  /// 选区清空时收起。
  void _onSelectionChanged() {
    if (!mounted) return;
    if (_terminalController.selection == null) {
      _selectionMenu.remove();
      return;
    }
    _selectionMenu.show(
      context: context,
      contextMenuBuilder: (context) => AdaptiveTextSelectionToolbar.buttonItems(
        anchors: _selectionAnchors(),
        buttonItems: [
          // 用官方的按钮类型，文案/样式由 Flutter 按平台给（不再硬编码中文）。
          ContextMenuButtonItem(
            type: ContextMenuButtonType.copy,
            onPressed: () {
              _selectionMenu.remove();
              _copySelection();
            },
          ),
          ContextMenuButtonItem(
            type: ContextMenuButtonType.paste,
            onPressed: () {
              _selectionMenu.remove();
              _pasteClipboard();
            },
          ),
        ],
      ),
    );
  }

  /// 选区端点 → 屏幕锚点。单元格尺寸用 fork 同一套字符度量算，
  /// 终端区原点取渲染盒的全局坐标（无滚动偏移，M2a 视口即缓冲）。
  TextSelectionToolbarAnchors _selectionAnchors() {
    final selection = _terminalController.selection!.normalized;
    final box =
        _terminalSurfaceKey.currentContext?.findRenderObject() as RenderBox?;
    if (box == null) {
      return const TextSelectionToolbarAnchors(primaryAnchor: Offset.zero);
    }
    final origin = box.localToGlobal(Offset.zero);
    final cell = calcCharSize(_style, MediaQuery.textScalerOf(context));
    final begin = selection.begin;
    final end = selection.end;
    return TextSelectionToolbarAnchors(
      primaryAnchor:
          origin + Offset(begin.x * cell.width, begin.y * cell.height),
      secondaryAnchor: origin +
          Offset((end.x + 1) * cell.width, (end.y + 1) * cell.height),
    );
  }

  /// 复制选区：取文在引擎里（Dart 不碰 BufferLine），发请求等 ClipboardText。
  void _copySelection() {
    if (_state != SessionState.connected) return;
    CopyRequest(sessionId: _sessionId).sendSignalToRust();
  }

  /// 全选当前视口（引擎的选区终点列是排除式的，所以终点取列数）。
  void _selectAll() {
    if (_state != SessionState.connected || _terminal.height == 0) return;
    final first = _terminal.stableRowAt(0);
    final last = _terminal.stableRowAt(_terminal.height - 1);
    if (first == null || last == null) return;
    SelectionRequest(
      sessionId: _sessionId,
      clear: false,
      anchorRow: first,
      anchorCol: 0,
      focusRow: last,
      focusCol: _terminal.viewWidth,
      rectangular: false,
    ).sendSignalToRust();
  }

  /// 系统剪贴板 → 远端（bracketed paste 等由 Rust 按远端模式处理）。
  Future<void> _pasteClipboard() async {
    final text = (await Clipboard.getData('text/plain'))?.text;
    if (text == null || text.isEmpty) return;
    _terminal.paste(text);
  }

  /// 发起（或重新发起）会话：换新编号，等终端量好几何后发 ConnectRequest。
  void _startSession() {
    _sessionId = _nextSessionId++;
    _promptsDismissed.value = false;
    _selectionEcho = null;
    _projectedFirstStable = null;
    _projectedLastStable = null;
    _terminalController.setExternalSelection(null, null);
    setState(() {
      _state = SessionState.connecting;
      _failure = FailureKind.none;
      _detail = '';
      _localNetworkSettingsUrl = '';
      // 不立刻发请求：等会话视图完成布局、拿到真实几何再发。
      _connectPending = true;
    });
    // 布局后几何变了 → fork 回调 _onTerminalResize 已经发出；没变（重连时
    // 视图尺寸通常不变，fork 不再回调）→ 这里用已量到的几何发出。
    SchedulerBinding.instance.addPostFrameCallback((_) => _flushPendingConnect());
  }

  bool _connectPending = false;

  /// 离开会话。连着的先确认，确认后断开并返回列表。
  Future<void> _leave() async {
    if (_state == SessionState.connected) {
      final confirmed = await showDialog<bool>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text('断开与「${widget.target.title}」的连接？'),
          actions: [
            TextButton(
              onPressed: () => Navigator.pop(context, false),
              child: const Text('取消'),
            ),
            FilledButton(
              onPressed: () => Navigator.pop(context, true),
              child: const Text('断开'),
            ),
          ],
        ),
      );
      if (confirmed != true || !mounted) return;
    }
    Navigator.of(context).pop();
  }

  Future<void> _openSettings(String url) async {
    await launchUrl(Uri.parse(url));
  }

  @override
  Widget build(BuildContext context) {
    // 连着的时候，系统返回（iOS 边缘右滑等）也走「确认后断开」。
    return PopScope(
      canPop: _state != SessionState.connected,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) _leave();
      },
      child: _buildSession(context),
    );
  }

  Widget _buildSession(BuildContext context) {
    final banner = switch (_state) {
      SessionState.connecting => _Banner(
          icon: Icons.sync,
          text: '正在连接 ${widget.target.title}…',
        ),
      SessionState.failed => _Banner(
          icon: Icons.error_outline,
          text: _failureText(_failure),
          detail: _detail,
          error: true,
          actions: [
            if (_localNetworkSettingsUrl.isNotEmpty)
              TextButton(
                onPressed: () => _openSettings(_localNetworkSettingsUrl),
                child: const Text('打开设置'),
              ),
            TextButton(onPressed: _startSession, child: const Text('重试')),
            TextButton(onPressed: _leave, child: const Text('返回')),
          ],
          hint: _localNetworkSettingsUrl.isEmpty
              ? null
              : '服务器在局域网内时，需要允许 GuoSSHell 访问本地网络。',
        ),
      SessionState.closed => _Banner(
          icon: Icons.link_off,
          text: '会话已结束',
          detail: _detail,
          actions: [
            TextButton(onPressed: _startSession, child: const Text('重新连接')),
            TextButton(onPressed: _leave, child: const Text('返回')),
          ],
        ),
      _ => null,
    };

    return Scaffold(
      backgroundColor: Colors.black,
      body: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Expanded(
              child: Stack(
                children: [
                  Positioned.fill(
                    child: Actions(
                      actions: {
                        _EngineCopyIntent: CallbackAction<_EngineCopyIntent>(
                          onInvoke: (_) => _copySelection(),
                        ),
                        _EngineSelectAllIntent: CallbackAction<_EngineSelectAllIntent>(
                          onInvoke: (_) => _selectAll(),
                        ),
                      },
                      child: _TerminalSurface(
                        key: _terminalSurfaceKey,
                        terminal: _terminal,
                        controller: _terminalController,
                        focusNode: _terminalFocus,
                        style: _style,
                      ),
                    ),
                  ),
                  // 诊断浮层与右上角关闭键已移除：它们悬在终端上方，
                  // 拖选区（尤其长选区）经过时会干扰触摸。
                  if (banner != null)
                    Positioned(top: 0, left: 0, right: 0, child: banner),
                ],
              ),
            ),
            TerminalKeyBar(
              terminal: _terminal,
              extraListen: _terminalController,
              canCopy: () => _terminalController.selection != null,
              onCopy: _copySelection,
              onPaste: _pasteClipboard,
              onToggleKeyboard: _toggleKeyboard,
              onDisconnect: _leave,
            ),
          ],
        ),
      ),
    );
  }
}

/// fork 的 TerminalView 需要有界高度（内部是 Scrollable）。
/// 会话期常驻（连接前也要完成首次布局，几何才能随 ConnectRequest 发出）。
class _TerminalSurface extends StatelessWidget {
  final FrameTerminal terminal;
  final TerminalController controller;
  final FocusNode focusNode;
  final TerminalStyle style;

  const _TerminalSurface({
    super.key,
    required this.terminal,
    required this.controller,
    required this.focusNode,
    required this.style,
  });

  @override
  Widget build(BuildContext context) {
    return TerminalView(
      terminal,
      controller: controller,
      focusNode: focusNode,
      autoResize: true,
      shortcuts: _terminalShortcuts,
      // iOS 软键盘的退格不产生硬件按键事件，必须靠编辑增量探测
      // （fork 的 onDelete → keyInput(backspace)）。
      deleteDetection: true,
      textStyle: style,
      theme: TerminalThemes.defaultTheme,
      keyboardType: TextInputType.emailAddress,
      keyboardAppearance: Brightness.dark,
    );
  }
}

/// 失败分类 → 文案。
String _failureText(FailureKind failure) => switch (failure) {
      FailureKind.none || FailureKind.other => '连接出错',
      FailureKind.notFound => '这条连接已不存在',
      FailureKind.invalidTarget => '连接目标无效：请检查主机、端口和用户名',
      FailureKind.authentication => '认证失败：用户名或密码不正确',
      FailureKind.hostKeyRejected => '已拒绝服务器的主机密钥',
      FailureKind.hostKeyChanged => '主机密钥已变更，连接已中止',
      FailureKind.network => '无法连接到服务器',
      FailureKind.timeout => '连接超时',
      FailureKind.keychain => '读写钥匙串失败',
    };

class _Banner extends StatelessWidget {
  final IconData icon;
  final String text;
  final String detail;
  final String? hint;
  final bool error;
  final List<Widget> actions;

  const _Banner({
    required this.icon,
    required this.text,
    this.detail = '',
    this.hint,
    this.error = false,
    this.actions = const [],
  });

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final foreground = error ? scheme.onErrorContainer : scheme.onSurface;
    return Material(
      color: error ? scheme.errorContainer : scheme.surface.withValues(alpha: 0.92),
      child: SafeArea(
        bottom: false,
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 8, 8, 4),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            mainAxisSize: MainAxisSize.min,
            children: [
              Row(
                children: [
                  Icon(icon, color: foreground, size: 20),
                  const SizedBox(width: 12),
                  Expanded(
                    child: Text(text, style: TextStyle(color: foreground)),
                  ),
                ],
              ),
              if (hint != null)
                Padding(
                  padding: const EdgeInsets.only(left: 32, top: 4),
                  child: Text(hint!, style: TextStyle(color: foreground, fontSize: 13)),
                ),
              if (detail.isNotEmpty)
                Padding(
                  padding: const EdgeInsets.only(left: 32, top: 2),
                  child: Text(
                    detail,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(
                      color: foreground.withValues(alpha: 0.7),
                      fontSize: 11,
                    ),
                  ),
                ),
              if (actions.isNotEmpty)
                TextButtonTheme(
                  data: TextButtonThemeData(
                    style: TextButton.styleFrom(foregroundColor: foreground),
                  ),
                  child: Row(mainAxisAlignment: MainAxisAlignment.end, children: actions),
                ),
            ],
          ),
        ),
      ),
    );
  }
}
