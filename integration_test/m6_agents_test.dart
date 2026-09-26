// M6 A 类：coding agent 在 App 里跑剧本（docs/acceptance-m6-2026-09-26.md §3 A）。
//
// --dart-define=M6_AGENTS=claude,codex,opencode   跑哪些 agent（默认全部）
// --dart-define=M6_SCENARIOS=stream,burst,…       跑哪些场景（默认全部；inline- 前缀 = 行内模式）
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:terminal_view/terminal_view.dart' show TerminalKey;

import 'm6_harness.dart';

const _agents = String.fromEnvironment('M6_AGENTS', defaultValue: 'claude,codex,opencode');
const _scenarios = String.fromEnvironment(
  'M6_SCENARIOS',
  defaultValue: 'stream,burst,tools,subagents,cjk,long,inline-stream,inline-cjk',
);

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;
  final report = <String, Object>{'device': m6Device};

  setUpAll(startRust);
  // 截图也存在 reportData 里（screenshots）：合并，不覆盖。
  tearDownAll(() => binding.reportData = {...?binding.reportData, 'agents': report});

  for (final agent in _agents.split(',')) {
    for (final entry in _scenarios.split(',')) {
      final inline = entry.startsWith('inline-');
      final scenario = inline ? entry.substring('inline-'.length) : entry;
      testWidgets('A：$agent × $entry', (tester) async {
        final app = M6App(tester, binding);
        final result = <String, Object>{};
        report['$agent/$entry'] = result;
        await app.open();
        await app.waitScreen('probe@');
        await app.type('clear\r');
        await app.waitIdle();

        final mark = app.perfMark;
        final started = DateTime.now();
        await app.type('m6-agent $agent $scenario${inline ? ' --inline' : ''}\r');

        if (scenario == 'stream' || scenario == 'burst') {
          // 流式中在 agent 的输入框里打字：从敲下最后一个字符到它出现在屏幕上。
          await tester.pump(const Duration(seconds: 2));
          const typed = 'zq6typ';
          final echo = Stopwatch()..start();
          await app.type(typed);
          await app.waitScreen(typed, timeout: const Duration(seconds: 10));
          result['typing_echo_ms'] = echo.elapsedMilliseconds;
        }
        if (scenario == 'subagents') {
          await tester.pump(const Duration(seconds: 3));
          await _switchViews(app, agent);
        }

        await app.waitScreen('M6-DONE', timeout: Duration(seconds: scenario == 'long' ? 240 : 120));
        result['duration_ms'] = DateTime.now().difference(started).inMilliseconds;
        await app.waitIdle(quiet: const Duration(milliseconds: 1500));
        result['perf'] = summarizePerf(app.perfSince(mark));
        await app.screenshot('agent-$agent-$entry');
        final garbage = screenGarbage(app.screenText());
        final problems = await app.screenMismatches();
        result['mismatches'] = problems;
        result['garbage'] = garbage;

        // 退出 agent：回到 shell，终端模式都复位（S5 的正常退出）。
        await _exitAgent(app, agent);
        await app.type('clear; m6-probe\r');
        await app.waitScreen('M6-MODES', timeout: const Duration(seconds: 10));
        final line = app.logicalLines().firstWhere((l) => l.contains('M6-MODES'));
        final modes = jsonDecode(line.substring(line.indexOf('{'))) as Map<String, dynamic>;
        result['modes_after'] = modes;

        expect(problems, isEmpty, reason: problems.join('\n'));
        expect(garbage, isEmpty, reason: garbage.join('\n'));
        expect(modes['1049'], 2, reason: '退出 agent 后不在备用屏');
        for (final mouse in ['1000', '1002', '1003']) {
          expect(modes[mouse], 2, reason: '鼠标上报 $mouse 应已关闭');
        }
        expect(modes['25'], 1, reason: '光标可见');
        await app.dispose();
      });
    }
  }

  testWidgets('S5：流式中直接杀掉 agent，回到 shell 后画面不卡住、reset 能恢复', (tester) async {
    final app = M6App(tester, binding);
    await app.open();
    await app.waitScreen('probe@');
    await app.type("clear; timeout --foreground -s KILL 4 m6-agent codex burst; echo M6-AGENT-KILLED-\$?\r");
    await app.waitScreen('M6-AGENT-KILLED-137', timeout: const Duration(seconds: 20));
    report['killed_left_alt_screen'] = app.pane.terminal.isUsingAltBuffer;
    await app.screenshot('agent-codex-killed');
    await app.type('reset; echo M6-RESET-DONE\r');
    await app.waitScreen('M6-RESET-DONE', timeout: const Duration(seconds: 10));
    expect(app.pane.terminal.isUsingAltBuffer, isFalse);
    await app.dispose();
  });
}

/// S3：subagent 还在刷新时切换视图。
Future<void> _switchViews(M6App app, String agent) async {
  final tester = app.tester;
  Future<void> shot(String name) => app.screenshot('agent-$agent-switch-$name');
  switch (agent) {
    case 'claude':
      // ↓ 进后台 agent 列表，Enter 查看选中的 agent，Esc 回到主视图。
      await app.key(TerminalKey.arrowDown);
      await tester.pump(const Duration(milliseconds: 600));
      await app.key(TerminalKey.enter);
      await tester.pump(const Duration(seconds: 1));
      await shot('child');
      await app.key(TerminalKey.escape);
      await tester.pump(const Duration(seconds: 1));
      await app.key(TerminalKey.keyO, ctrl: true);
      await tester.pump(const Duration(seconds: 1));
      await shot('transcript');
      await app.key(TerminalKey.keyO, ctrl: true);
    case 'codex':
      // 输入框为空时 Alt+← / → 在 agent 线程之间切换。
      for (var i = 0; i < 2; i++) {
        await app.key(TerminalKey.arrowRight, alt: true);
        await tester.pump(const Duration(milliseconds: 800));
      }
      await shot('child');
      for (var i = 0; i < 2; i++) {
        await app.key(TerminalKey.arrowLeft, alt: true);
        await tester.pump(const Duration(milliseconds: 800));
      }
    case 'opencode':
      // Ctrl+X ↓ 进第一个子会话，← / → 在子会话之间切换，↑ 回父会话。
      await app.key(TerminalKey.keyX, ctrl: true);
      await app.key(TerminalKey.arrowDown);
      await tester.pump(const Duration(seconds: 1));
      await shot('child');
      for (final key in [TerminalKey.arrowRight, TerminalKey.arrowRight, TerminalKey.arrowLeft]) {
        await app.key(TerminalKey.keyX, ctrl: true);
        await app.key(key);
        await tester.pump(const Duration(milliseconds: 800));
      }
      await app.key(TerminalKey.keyX, ctrl: true);
      await app.key(TerminalKey.arrowUp);
      await tester.pump(const Duration(seconds: 1));
  }
}

/// 退出 agent，等回到 shell 提示符。
Future<void> _exitAgent(M6App app, String agent) async {
  final tester = app.tester;
  bool atShell() => !app.pane.terminal.isUsingAltBuffer && app.screen().reversed.any((l) => l.startsWith('probe@'));
  for (var attempt = 0; attempt < 6 && !atShell(); attempt++) {
    if (agent == 'opencode' && attempt.isOdd) {
      await app.key(TerminalKey.keyD, ctrl: true);
    } else {
      await app.key(TerminalKey.keyC, ctrl: true);
    }
    await tester.pump(const Duration(milliseconds: 700));
  }
  await app.waitFor(atShell, '$agent 退回 shell', timeout: const Duration(seconds: 15));
}
