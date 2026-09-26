import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:guosh_shell/src/settings/key_bar_editor.dart';
import 'package:guosh_shell/src/terminal/key_bar_layout.dart';

void main() {
  testWidgets('排布可删除、跨排移动、添加，保存草稿不修改原数据', (tester) async {
    const initial = [
      ['escape', 'tab'],
      ['copy'],
    ];
    List<List<String>>? saved;
    await tester.pumpWidget(
      MaterialApp(
        home: KeyBarEditor(
          initialRows: initial,
          onSave: (rows) async {
            saved = rows;
          },
        ),
      ),
    );
    await tester.tap(find.byTooltip('删除按钮').first);
    await tester.pump();
    expect(find.text('Esc'), findsNothing);
    await tester.tap(find.byTooltip('移到第二排').first);
    await tester.pump();
    expect(find.textContaining('这一排没有按钮'), findsOneWidget);
    await tester.tap(find.text('添加按钮'));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(ActionChip, 'F1'));
    await tester.pumpAndSettle();
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(saved, [
      ['f1'],
      ['copy', 'tab'],
    ]);
    expect(initial, [
      ['escape', 'tab'],
      ['copy'],
    ]);
  });

  testWidgets('空排布仍可恢复默认，保存失败保留草稿', (tester) async {
    await tester.pumpWidget(
      MaterialApp(
        home: KeyBarEditor(
          initialRows: const [[], []],
          onSave: (_) async => throw StateError('disk'),
        ),
      ),
    );
    await tester.tap(find.text('恢复默认'));
    await tester.pump();
    expect(find.text('Esc'), findsOneWidget);
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(find.text('保存失败，请重试。'), findsOneWidget);
    expect(find.text('Esc'), findsOneWidget);
  });

  testWidgets('拖动手柄改变排序，同名按钮仍有独立身份', (tester) async {
    List<List<String>>? saved;
    await tester.pumpWidget(
      MaterialApp(
        home: KeyBarEditor(
          initialRows: const [
            ['escape', 'tab', 'tab'],
            [],
          ],
          onSave: (rows) async {
            saved = rows;
          },
        ),
      ),
    );
    final handle = find.byType(ReorderableDragStartListener).first;
    final start = tester.getCenter(handle);
    final target = Offset(
      start.dx,
      tester.getRect(find.byType(ListTile).last).bottom + 40,
    );
    final gesture = await tester.startGesture(start);
    for (var step = 1; step <= 20; step++) {
      await gesture.moveTo(Offset.lerp(start, target, step / 20)!);
      await tester.pump(const Duration(milliseconds: 20));
    }
    await tester.pump(const Duration(milliseconds: 300));
    await gesture.up();
    await tester.pumpAndSettle();
    await tester.tap(find.text('保存'));
    await tester.pumpAndSettle();
    expect(saved, [
      ['tab', 'tab', 'escape'],
      [],
    ]);
  });

  test('默认按钮和快捷键都有定义', () {
    for (final id in defaultKeyBarRows.expand((row) => row)) {
      expect(keyBarButton(id), isNotNull);
    }
    expect(keyBarButton('text:ls -la')!.text, 'ls -la');
    expect(keyBarButton('ctrlC')!.ctrl, isTrue);
  });
}
