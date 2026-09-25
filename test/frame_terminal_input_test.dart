import 'package:flutter_test/flutter_test.dart';
import 'package:guosh_shell/src/terminal/frame_terminal.dart';
import 'package:terminal_view/terminal_view.dart';

void main() {
  group('键名（与 session.rs 的 parse_key_code 对齐）', () {
    test('数字键按 HID 顺序映射：digit1..digit9、digit0', () {
      const digits = [
        TerminalKey.digit1,
        TerminalKey.digit2,
        TerminalKey.digit3,
        TerminalKey.digit4,
        TerminalKey.digit5,
        TerminalKey.digit6,
        TerminalKey.digit7,
        TerminalKey.digit8,
        TerminalKey.digit9,
        TerminalKey.digit0,
      ];
      expect(
        digits.map(FrameTerminal.keyName).toList(),
        [for (final d in '1234567890'.split('')) 'character:$d'],
      );
    });

    test('字母键小写、F 键用 fN', () {
      expect(FrameTerminal.keyName(TerminalKey.keyA), 'character:a');
      expect(FrameTerminal.keyName(TerminalKey.keyZ), 'character:z');
      expect(FrameTerminal.keyName(TerminalKey.f1), 'f1');
      expect(FrameTerminal.keyName(TerminalKey.f12), 'f12');
      expect(FrameTerminal.keyName(TerminalKey.escape), 'escape');
    });
  });

  group('输入路由', () {
    late List<TerminalInputEvent> events;
    late FrameTerminal terminal;

    setUp(() {
      events = [];
      terminal = FrameTerminal(onInput: (event) {
        events.add(event);
        return true;
      });
    });

    test('无 Ctrl/Alt 的可打印键交回文本通道（保住大小写）', () {
      // fork 的软键盘路径把 'A' 和 'a' 都映射成 keyA：这里必须拒收，
      // fork 才会改走 textInput(原字符)。
      expect(terminal.keyInput(TerminalKey.keyA), isFalse);
      expect(terminal.keyInput(TerminalKey.digit9), isFalse);
      expect(terminal.keyInput(TerminalKey.keyA, shift: true), isFalse);
      expect(events, isEmpty);

      terminal.textInput('A');
      expect(events.single, isA<TextInputEvent>().having((e) => e.text, 'text', 'A'));
    });

    test('硬件 Ctrl 组合键以键的形式发出', () {
      expect(terminal.keyInput(TerminalKey.keyC, ctrl: true), isTrue);
      expect(
        events.single,
        isA<KeyInputEvent>()
            .having((e) => e.key, 'key', 'character:c')
            .having((e) => e.control, 'control', isTrue),
      );
    });

    test('功能键与方向键照常发出', () {
      expect(terminal.keyInput(TerminalKey.f5), isTrue);
      expect(terminal.keyInput(TerminalKey.arrowUp), isTrue);
      expect(events.map((e) => (e as KeyInputEvent).key), ['f5', 'arrow_up']);
    });

    test('挂住的 Ctrl 作用于下一个字符，字符原样保留', () {
      terminal.tapModifier('ctrl');
      terminal.textInput('C');
      expect(
        events.single,
        isA<KeyInputEvent>()
            .having((e) => e.key, 'key', 'character:C')
            .having((e) => e.control, 'control', isTrue),
      );
      // 挂住一次即消耗。
      terminal.textInput('c');
      expect(events.last, isA<TextInputEvent>());
    });
  });

  group('几何', () {
    test('单格像素换算成整个终端的像素，且只在变化时回调', () {
      final reported = <TerminalGeometry>[];
      final terminal = FrameTerminal(onResize: reported.add);

      terminal.resize(80, 24, 9, 18);
      terminal.resize(80, 24, 9, 18); // 同一几何：不重复回调
      expect(reported, [
        const TerminalGeometry(cols: 80, rows: 24, pixelWidth: 720, pixelHeight: 432),
      ]);
      expect(terminal.measuredGeometry, reported.single);

      terminal.resize(100, 30, 9, 18);
      expect(reported.last,
          const TerminalGeometry(cols: 100, rows: 30, pixelWidth: 900, pixelHeight: 540));
    });
  });
}
