// M6 E 类：终端协议与宽字符（docs/acceptance-m6-2026-09-26.md §3 E、S7）。
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

import 'm6_harness.dart';

void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  binding.framePolicy = LiveTestWidgetsFlutterBindingFramePolicy.fullyLive;
  final report = <String, Object>{'device': m6Device};

  setUpAll(startRust);
  // 截图也存在 reportData 里（screenshots）：合并，不覆盖。
  tearDownAll(() => binding.reportData = {...?binding.reportData, 'protocol': report});

  testWidgets('S7 / E：宽字符按列排布，画面与引擎一致', (tester) async {
    final app = M6App(tester, binding);
    await app.open();
    await app.waitScreen('probe@');
    await app.type('clear; m6-cjk\r');
    await app.waitScreen('M6-CJK-END');
    await app.waitIdle();

    final problems = await app.screenMismatches();
    report['cjk_mismatches'] = problems;
    final cols = app.pane.terminal.viewWidth;
    final lines = app.screen();
    expect(lines, contains('中' * (cols ~/ 2)), reason: '一整行中文要铺满');
    expect(lines.any((l) => l.startsWith('中文X tail 🚀Y end')), isTrue, reason: lines.join('\n'));
    expect(screenGarbage(app.screenText()), isEmpty);
    await app.screenshot('protocol-cjk');
    expect(problems, isEmpty, reason: problems.join('\n'));
    await app.dispose();
  });

  testWidgets('E1：同步输出——成对整帧出现；没有结束序列或被杀也照常显示', (tester) async {
    final app = M6App(tester, binding);
    await app.open();
    await app.waitScreen('probe@');

    await app.type('clear; m6-sync stall\r');
    final visible = await app.waitScreen('M6-SYNC-VISIBLE', timeout: const Duration(seconds: 3));
    report['sync_stall_visible_ms'] = visible.inMilliseconds;
    expect(visible, lessThan(const Duration(milliseconds: 1500)), reason: 'BSU 后 150 ms 应照常显示，不该等到 2 秒后');
    await app.waitScreen('M6-SYNC-STALL-DONE');

    await app.type('clear; m6-sync kill\r');
    await app.waitScreen('M6-SYNC-KILL-DONE', timeout: const Duration(seconds: 5));
    await app.type('echo after-kill-$m6Device\r');
    await app.waitScreen('after-kill-$m6Device', timeout: const Duration(seconds: 5));

    await app.type('clear; m6-sync pair\r');
    await app.waitScreen('M6-SYNC-PAIR-DONE', timeout: const Duration(seconds: 20));
    await app.waitIdle();
    final problems = await app.screenMismatches();
    expect(problems, isEmpty, reason: problems.join('\n'));
    await app.dispose();
  });

  testWidgets('E2：终端查询按顺序应答，TUI 常用的模式都认得', (tester) async {
    final app = M6App(tester, binding);
    await app.open();
    await app.waitScreen('probe@');
    await app.type('clear; m6-probe\r');
    await app.waitScreen('M6-MODES');
    final line = app.logicalLines().firstWhere((l) => l.contains('M6-MODES'));
    final modes = jsonDecode(line.substring(line.indexOf('{'))) as Map<String, dynamic>;
    report['probe_modes'] = modes;
    expect(modes['da1'], isTrue);
    expect(modes['cpr'], isA<List>());
    for (final mode in ['1', '25', '1000', '1002', '1003', '1004', '1006', '1049', '2004', '2026']) {
      expect(modes[mode], isNot(0), reason: '模式 $mode 应被识别（DECRQM）');
    }
    expect(modes['1049'], 2, reason: '不在备用屏');
    expect(modes['25'], 1, reason: '光标可见');
    await app.dispose();
  });
}
