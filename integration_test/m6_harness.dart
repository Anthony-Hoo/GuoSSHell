// M6 集成测试的公共部分（docs/acceptance-m6-2026-09-26.md §2.6）：在模拟器 / macOS 上驱动
// 真实 App，连验收服务器（scripts/sshd-test.sh up），读 App 自己的终端缓冲断言。
//
// 由 scripts/m6.sh 经 flutter drive 运行；--dart-define：
//   M6_HOST / M6_PORT   验收服务器（默认 127.0.0.1:2223）
//   M6_DEVICE           设备名，截图与报告按它分目录
import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:guosh_shell/src/bindings/bindings.dart';
import 'package:guosh_shell/src/terminal/perf_monitor.dart';
import 'package:guosh_shell/src/terminal/session_target.dart';
import 'package:guosh_shell/src/terminal/terminal_pane.dart';
import 'package:guosh_shell/src/workspace/workspace_page.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';
import 'package:rinf/rinf.dart';
import 'package:terminal_view/terminal_view.dart' show BufferLine, TerminalKey;

const m6Host = String.fromEnvironment('M6_HOST', defaultValue: '127.0.0.1');
const m6Port = int.fromEnvironment('M6_PORT', defaultValue: 2223);
const m6Device = String.fromEnvironment('M6_DEVICE', defaultValue: 'device');

bool _rustStarted = false;

/// 起 Rust 侧并等设置到位（与 App 启动时的 _StartupGate 相同的顺序）。
Future<void> startRust() async {
  if (_rustStarted) return;
  _rustStarted = true;
  await initializeRust(assignRustSignal);
  final ready = AppReady.rustSignalStream.first;
  final settings = SettingsState.rustSignalStream.first;
  final directory = await getApplicationSupportDirectory();
  AppStart(supportDir: directory.path).sendSignalToRust();
  final ok = (await ready).message;
  if (!ok.ok) throw StateError('AppReady: ${ok.detail}');
  SettingsQuery().sendSignalToRust();
  await settings;
}

/// 一次测试里的 App：一个工作区、若干窗格。
class M6App {
  M6App(this.tester, this.binding) {
    _frames = FrameUpdate.rustSignalStream.listen((pack) {
      _lastFrame[pack.message.sessionId] = DateTime.now();
    });
  }

  final WidgetTester tester;
  final IntegrationTestWidgetsFlutterBinding binding;
  late final StreamSubscription<RustSignalPack<FrameUpdate>> _frames;
  final Map<int, DateTime> _lastFrame = {};

  /// 打开工作区并连上验收服务器（首次连接自动信任主机密钥，密钥变了就替换）。
  Future<TerminalPaneController> open({String command = ''}) async {
    await tester.pumpWidget(MaterialApp(
      theme: ThemeData(brightness: Brightness.dark, colorScheme: ColorScheme.fromSeed(seedColor: Colors.teal, brightness: Brightness.dark)),
      home: WorkspacePage(
        initial: SessionTarget.quick(
          host: m6Host,
          port: m6Port,
          username: 'probe',
          password: 'probe',
          command: command,
        ),
      ),
    ));
    return waitConnected();
  }

  /// 活动窗格（第一个）。
  TerminalPaneController get pane => panes.first;

  List<TerminalPaneController> get panes =>
      tester.widgetList<TerminalPane>(find.byType(TerminalPane)).map((w) => w.controller).toList();

  /// 等窗格连上；路上遇到主机密钥确认就替用户点掉。
  Future<TerminalPaneController> waitConnected({TerminalPaneController? of}) async {
    final deadline = DateTime.now().add(const Duration(seconds: 60));
    while (true) {
      await tester.pump(const Duration(milliseconds: 100));
      if (find.text('信任并连接').evaluate().isNotEmpty) {
        await tester.tap(find.text('信任并连接'));
        continue;
      }
      if (find.text('替换旧密钥并连接').evaluate().isNotEmpty) {
        final checkbox = find.byType(Checkbox);
        if (checkbox.evaluate().isNotEmpty) await tester.tap(checkbox.first);
        await tester.pump(const Duration(milliseconds: 300));
        final replace = find.text('替换旧密钥并连接');
        if (replace.evaluate().isNotEmpty) await tester.tap(replace.first);
        continue;
      }
      final panes = this.panes;
      final pane = of ?? (panes.isEmpty ? null : panes.first);
      if (pane != null && pane.connected) return pane;
      if (DateTime.now().isAfter(deadline)) {
        throw TestFailure('连接验收服务器超时（$m6Host:$m6Port，先 ./scripts/sshd-test.sh up）');
      }
    }
  }

  /// 在窗格里敲一段文本（经 App 的文本输入通道，等同软键盘提交）；`\r` 即回车。
  Future<void> type(String text, {TerminalPaneController? pane}) async {
    (pane ?? this.pane).terminal.textInput(text);
    await tester.pump(const Duration(milliseconds: 50));
  }

  /// 按一个功能键（经 App 的键编码通道）。
  Future<void> key(TerminalKey key, {bool ctrl = false, bool alt = false, bool shift = false, TerminalPaneController? pane}) async {
    (pane ?? this.pane).terminal.keyInput(key, ctrl: ctrl, alt: alt, shift: shift);
    await tester.pump(const Duration(milliseconds: 50));
  }

  /// 当前屏幕（不含滚回）的每一行文本，宽字符的第二列不重复。
  List<String> screen({TerminalPaneController? pane}) {
    final terminal = (pane ?? this.pane).terminal;
    final top = terminal.screenTopIndex;
    return [
      for (var row = 0; row < terminal.viewHeight; row++) _lineText(terminal.lineAt(top + row), terminal.viewWidth),
    ];
  }

  String screenText({TerminalPaneController? pane}) => screen(pane: pane).join('\n');

  /// 当前屏幕按软换行接回的逻辑行（一行输出比屏幕宽时跨几行）。
  List<String> logicalLines({TerminalPaneController? pane}) {
    final terminal = (pane ?? this.pane).terminal;
    final top = terminal.screenTopIndex;
    final lines = <String>[];
    var current = StringBuffer();
    for (var row = 0; row < terminal.viewHeight; row++) {
      final line = terminal.lineAt(top + row);
      final text = _lineText(line, terminal.viewWidth, trim: !line.isWrapped);
      current.write(text);
      if (!line.isWrapped) {
        lines.add(current.toString());
        current = StringBuffer();
      }
    }
    if (current.isNotEmpty) lines.add(current.toString());
    return lines;
  }

  /// 终端状态（等待超时时一并打出，便于判断是画面问题还是测试问题）。
  String describe({TerminalPaneController? pane}) {
    final controller = pane ?? this.pane;
    final t = controller.terminal;
    return '会话 ${controller.sessionId} ${controller.state} · ${t.viewWidth}x${t.viewHeight} · '
        '行数 ${t.height} · 屏幕首行 ${t.screenTopIndex} · 最早一行 ${t.firstStableRow} · '
        '窗口盖住屏幕 ${t.windowCovers(t.screenTopIndex, t.viewHeight)} · 备用屏 ${t.isUsingAltBuffer}';
  }

  /// 等屏幕上出现 [text]（或满足 [test]）。
  Future<Duration> waitScreen(String text, {Duration timeout = const Duration(seconds: 60), TerminalPaneController? pane}) async {
    final start = DateTime.now();
    final deadline = start.add(timeout);
    while (!screenText(pane: pane).contains(text)) {
      if (DateTime.now().isAfter(deadline)) {
        throw TestFailure('等「$text」超时。${describe(pane: pane)}\n当前屏幕：\n${screenText(pane: pane)}');
      }
      await tester.pump(const Duration(milliseconds: 50));
    }
    return DateTime.now().difference(start);
  }

  /// 等条件成立。
  Future<void> waitFor(bool Function() done, String what, {Duration timeout = const Duration(seconds: 30)}) async {
    final deadline = DateTime.now().add(timeout);
    while (!done()) {
      if (DateTime.now().isAfter(deadline)) throw TestFailure('等「$what」超时。${describe()}\n当前屏幕：\n${screenText()}');
      await tester.pump(const Duration(milliseconds: 50));
    }
  }

  /// 等输出静止：[quiet] 内没有新帧。
  Future<void> waitIdle({Duration quiet = const Duration(milliseconds: 800), TerminalPaneController? pane}) async {
    final sessionId = (pane ?? this.pane).sessionId;
    final deadline = DateTime.now().add(const Duration(seconds: 60));
    while (true) {
      await tester.pump(const Duration(milliseconds: 100));
      final last = _lastFrame[sessionId];
      if (last == null || DateTime.now().difference(last) >= quiet) return;
      if (DateTime.now().isAfter(deadline)) return;
    }
  }

  /// 画面一致性：引擎按列给出的当前屏幕与 Dart 行池逐列比对，返回不一致之处（空 = 一致）。
  /// 屏幕还在刷新时（TUI 定时重画）两边可能正好差一帧：比对几次，只报告每次都在的差异。
  Future<List<String>> screenMismatches({TerminalPaneController? pane, int attempts = 4}) async {
    List<String> problems = const [];
    for (var attempt = 0; attempt < attempts; attempt++) {
      problems = await _compareOnce(pane ?? this.pane);
      if (problems.isEmpty) return problems;
      await tester.pump(const Duration(milliseconds: 300));
    }
    return problems;
  }

  Future<List<String>> _compareOnce(TerminalPaneController controller) async {
    final reply = ScreenCheck.rustSignalStream.firstWhere((p) => p.message.sessionId == controller.sessionId);
    ScreenCheckRequest(sessionId: controller.sessionId).sendSignalToRust();
    final check = (await reply.timeout(const Duration(seconds: 10))).message;
    // 在途的帧先画上（引擎回报之前发出的帧，Dart 可能还没处理）。
    await tester.pump(const Duration(milliseconds: 100));
    final terminal = controller.terminal;
    final top = terminal.screenTopIndex;
    final problems = <String>[];
    for (var row = 0; row < check.rows.length; row++) {
      final engine = check.rows[row];
      final line = terminal.lineAt(top + row);
      for (var col = 0; col < check.cols; col++) {
        final expected = _blank(col < engine.length ? engine[col] : '');
        final actual = _blank(_cellText(line, col));
        if (expected != actual) {
          problems.add('第 $row 行第 $col 列：引擎「$expected」App「$actual」');
          if (problems.length > 20) return problems;
        }
      }
    }
    return problems;
  }

  /// 截图（flutter drive 的驱动端落盘到 build/m6/screenshots/<设备>/）。
  Future<void> screenshot(String name) async {
    await tester.pump();
    await binding.takeScreenshot('$m6Device/$name');
  }

  /// 从 [since] 起收集到的性能窗口（PerfMonitor）。
  List<PerfRecord> perfSince(int since, {TerminalPaneController? pane}) {
    final sessionId = (pane ?? this.pane).sessionId;
    return PerfMonitor.instance.records.skip(since).where((r) => r.sessionId == sessionId).toList();
  }

  int get perfMark => PerfMonitor.instance.records.length;

  Future<void> dispose() async {
    await _frames.cancel();
    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump(const Duration(milliseconds: 300));
  }
}

/// 把几窗性能数据汇总成一条（写进报告）。
Map<String, Object> summarizePerf(List<PerfRecord> records) {
  if (records.isEmpty) return {'windows': 0};
  int maxOf(int Function(PerfRecord) f) => records.map(f).reduce((a, b) => a > b ? a : b);
  int sumOf(int Function(PerfRecord) f) => records.map(f).fold(0, (a, b) => a + b);
  final frames = sumOf((r) => r.frames);
  final windowMs = sumOf((r) => r.windowMs);
  return {
    'windows': records.length,
    'fps': windowMs == 0 ? 0 : double.parse((frames * 1000 / windowMs).toStringAsFixed(1)),
    'latency_us_p95_max': maxOf((r) => r.latencyUsP95),
    'latency_us_max': maxOf((r) => r.latencyUsMax),
    'ack_timeouts': sumOf((r) => r.ackTimeouts),
    'sync_timeouts': sumOf((r) => r.syncTimeouts),
    'render_us_max': maxOf((r) => r.renderUsMax),
    'apply_us_max': maxOf((r) => r.applyUsMax),
    'raster_us_p90_max': maxOf((r) => r.rasterUsP90),
    'build_us_p90_max': maxOf((r) => r.buildUsP90),
    'janky': sumOf((r) => r.janky),
    'flutter_frames': sumOf((r) => r.flutterFrames),
    'input_bytes': sumOf((r) => r.inputBytes),
    'rss_mb_max': maxOf((r) => r.rssBytes ~/ (1 << 20)),
  };
}

/// 屏幕上不该出现的东西：替换字符、泄漏成文字的转义序列。
List<String> screenGarbage(String screen) => [
      if (screen.contains('�')) '出现替换字符 U+FFFD',
      for (final m in RegExp(r'\[\?\d+[hl]|\]\d+;|\[\d+;\d+[Hr]').allMatches(screen).take(3)) '疑似转义序列泄漏：「${m.group(0)}」',
    ];

/// 旋转 / 改窗口的替身：改视图的物理尺寸（框架层；真实旋转在 XCUITest 与 iPhone 上验）。
Future<void> resizeView(WidgetTester tester, Size logical) async {
  final view = tester.view;
  view.physicalSize = logical * view.devicePixelRatio;
  await tester.pump(const Duration(milliseconds: 600));
}

Future<void> restoreView(WidgetTester tester) async {
  tester.view.resetPhysicalSize();
  await tester.pump(const Duration(milliseconds: 600));
}

Future<void> setOrientation(List<DeviceOrientation> orientations) =>
    SystemChrome.setPreferredOrientations(orientations);

String _cellText(BufferLine line, int col) {
  if (col >= line.length) return '';
  final code = line.getCodePoint(col);
  if (code == 0) return '';
  return String.fromCharCode(code) + (line.getCombined(col) ?? '');
}

String _lineText(BufferLine line, int cols, {bool trim = true}) {
  final out = StringBuffer();
  for (var col = 0; col < cols; col++) {
    final text = _cellText(line, col);
    if (text.isNotEmpty) {
      out.write(text);
    } else if (col == 0 || line.getCodePoint(col - 1) == 0 || _width(line, col - 1) == 1) {
      out.write(' ');
    }
  }
  return trim ? out.toString().trimRight() : out.toString();
}

int _width(BufferLine line, int col) => line.getWidth(col);

String _blank(String text) => text == ' ' ? '' : text;
