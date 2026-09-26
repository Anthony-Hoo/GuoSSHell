import 'package:flutter/material.dart';

import '../terminal/key_bar_layout.dart';

/// 排布修改先留在页面，保存成功后才应用；退出即放弃草稿。
class KeyBarEditor extends StatefulWidget {
  final List<List<String>> initialRows;
  final Future<void> Function(List<List<String>>) onSave;

  const KeyBarEditor({
    super.key,
    required this.initialRows,
    required this.onSave,
  });

  @override
  State<KeyBarEditor> createState() => _KeyBarEditorState();
}

/// 每个按钮有独立身份，同名按钮和跨排移动也保持拖拽状态稳定。
class _ButtonEntry {
  _ButtonEntry(this.id);
  final String id;
}

class _KeyBarEditorState extends State<KeyBarEditor> {
  late List<List<_ButtonEntry>> _rows = [
    for (var i = 0; i < 2; i++)
      [
        for (final id in widget.initialRows.elementAtOrNull(i) ?? <String>[])
          _ButtonEntry(id),
      ],
  ];
  int _row = 0;
  bool _saving = false;
  String? _error;

  Future<void> _save() async {
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      await widget.onSave([
        for (final row in _rows) [for (final entry in row) entry.id],
      ]);
      if (mounted) Navigator.pop(context);
    } catch (_) {
      if (mounted) {
        setState(() {
          _saving = false;
          _error = '保存失败，请重试。';
        });
      }
    }
  }

  Future<void> _add() async {
    if (_rows[_row].length >= 24) return;
    final selected = await showModalBottomSheet<String>(
      context: context,
      isScrollControlled: true,
      showDragHandle: true,
      builder: (context) => SafeArea(
        child: SizedBox(
          height: MediaQuery.sizeOf(context).height * 0.65,
          child: ListView(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 20),
            children: [
              Text('添加按钮', style: Theme.of(context).textTheme.titleLarge),
              const SizedBox(height: 16),
              Wrap(
                spacing: 8,
                runSpacing: 8,
                children: [
                  for (final button in keyBarButtons)
                    ActionChip(
                      label: Text(button.label),
                      onPressed: () => Navigator.pop(context, button.id),
                    ),
                ],
              ),
              const SizedBox(height: 16),
              ListTile(
                leading: const Icon(Icons.text_fields),
                title: const Text('自定义文本'),
                subtitle: const Text('点击后输入文本，不自动发送回车'),
                onTap: () => Navigator.pop(context, 'custom'),
              ),
            ],
          ),
        ),
      ),
    );
    if (!mounted || selected == null) return;
    var id = selected;
    if (selected == 'custom') {
      final text = await showDialog<String>(
        context: context,
        builder: (_) => const _CustomTextDialog(),
      );
      if (!mounted || text == null) return;
      id = 'text:$text';
    }
    setState(() => _rows[_row].add(_ButtonEntry(id)));
  }

  @override
  Widget build(BuildContext context) {
    final row = _rows[_row];
    return PopScope(
      canPop: !_saving,
      child: Scaffold(
        appBar: AppBar(
          title: const Text('编辑功能按钮'),
          actions: [
            TextButton(
              onPressed: _saving ? null : _save,
              child: Text(_saving ? '保存中…' : '保存'),
            ),
          ],
        ),
        body: SafeArea(
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.all(16),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text('拖动右侧手柄排序，可将按钮移到另一排。每排最多 24 个。'),
                    const SizedBox(height: 12),
                    SegmentedButton<int>(
                      segments: const [
                        ButtonSegment(value: 0, label: Text('第一排')),
                        ButtonSegment(value: 1, label: Text('第二排')),
                      ],
                      selected: {_row},
                      onSelectionChanged: _saving
                          ? null
                          : (v) => setState(() => _row = v.single),
                    ),
                    if (_error != null)
                      Text(
                        _error!,
                        style: TextStyle(
                          color: Theme.of(context).colorScheme.error,
                        ),
                      ),
                  ],
                ),
              ),
              Expanded(
                child: row.isEmpty
                    ? const Center(child: Text('这一排没有按钮，点击下方“添加按钮”。'))
                    : ReorderableListView.builder(
                        buildDefaultDragHandles: false,
                        itemCount: row.length,
                        onReorderItem: (oldIndex, newIndex) {
                          if (_saving) return;
                          setState(() {
                            row.insert(newIndex, row.removeAt(oldIndex));
                          });
                        },
                        itemBuilder: (context, index) => ListTile(
                          key: ObjectKey(row[index]),
                          title: Text(
                            keyBarButton(row[index].id)?.label ?? row[index].id,
                          ),
                          trailing: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              IconButton(
                                tooltip: '移到${_row == 0 ? '第二' : '第一'}排',
                                icon: const Icon(Icons.swap_vert),
                                onPressed:
                                    _saving || _rows[1 - _row].length >= 24
                                    ? null
                                    : () => setState(
                                        () => _rows[1 - _row].add(
                                          row.removeAt(index),
                                        ),
                                      ),
                              ),
                              IconButton(
                                tooltip: '删除按钮',
                                icon: const Icon(Icons.remove_circle_outline),
                                onPressed: _saving
                                    ? null
                                    : () => setState(() => row.removeAt(index)),
                              ),
                              ReorderableDragStartListener(
                                index: index,
                                enabled: !_saving,
                                child: const Padding(
                                  padding: EdgeInsets.all(12),
                                  child: Icon(Icons.drag_handle),
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
              ),
              Padding(
                padding: const EdgeInsets.all(16),
                child: Wrap(
                  spacing: 12,
                  runSpacing: 8,
                  children: [
                    FilledButton.icon(
                      onPressed: _saving || row.length >= 24 ? null : _add,
                      icon: const Icon(Icons.add),
                      label: const Text('添加按钮'),
                    ),
                    TextButton(
                      onPressed: _saving
                          ? null
                          : () => setState(
                              () => _rows = [
                                for (final r in defaultKeyBarRows)
                                  [for (final id in r) _ButtonEntry(id)],
                              ],
                            ),
                      child: const Text('恢复默认'),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _CustomTextDialog extends StatefulWidget {
  const _CustomTextDialog();
  @override
  State<_CustomTextDialog> createState() => _CustomTextDialogState();
}

class _CustomTextDialogState extends State<_CustomTextDialog> {
  final _text = TextEditingController();
  @override
  void dispose() {
    _text.dispose();
    super.dispose();
  }

  bool get _valid =>
      _text.text.trim().isNotEmpty &&
      _text.text.runes.length <= 32 &&
      !_text.text.contains(RegExp(r'[\x00-\x1f\x7f-\x9f]'));
  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('自定义文本按钮'),
    content: TextField(
      controller: _text,
      autofocus: true,
      maxLength: 32,
      decoration: const InputDecoration(
        labelText: '输入文本',
        helperText: '不自动发送回车',
      ),
      onChanged: (_) => setState(() {}),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('取消'),
      ),
      FilledButton(
        onPressed: _valid ? () => Navigator.pop(context, _text.text) : null,
        child: const Text('添加'),
      ),
    ],
  );
}
