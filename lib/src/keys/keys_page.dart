import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../bindings/bindings.dart';
import 'card_scan_page.dart';
import 'key_import_page.dart';
import 'key_requests.dart';
import 'name_dialog.dart';

/// 私钥：列表、导入私钥、登记 OpenPGP 卡、查看公钥、改名、删除，以及 iCloud 钥匙串同步开关。
class KeysPage extends StatefulWidget {
  const KeysPage({super.key});

  @override
  State<KeysPage> createState() => _KeysPageState();
}

class _KeysPageState extends State<KeysPage> {
  StreamSubscription? _sub;
  KeyListState? _state = KeyListState.latestRustSignal?.message;
  bool _syncBusy = false;

  @override
  void initState() {
    super.initState();
    _sub = KeyListState.rustSignalStream.listen((pack) {
      if (mounted) setState(() => _state = pack.message);
    });
    KeyQuery().sendSignalToRust();
  }

  @override
  void dispose() {
    _sub?.cancel();
    super.dispose();
  }

  void _snack(String message) {
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(message)));
  }

  Future<void> _run(Future<KeyResult> Function() request) async {
    String? message;
    try {
      final result = await request();
      if (result.error != KeyError.none) message = keyErrorText(result.error);
    } on TimeoutException {
      message = '操作超时';
    }
    if (message != null && mounted) _snack(message);
  }

  Future<void> _import() async {
    await Navigator.of(context).push<String>(MaterialPageRoute(
      builder: (_) => const KeyImportPage(),
    ));
  }

  Future<void> _addCard() async {
    await Navigator.of(context).push<String>(MaterialPageRoute(
      builder: (_) => const CardScanPage(),
    ));
  }

  /// 开关同步前讲清楚后果，用户同意才动。
  Future<void> _toggleSync(bool enabled) async {
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(enabled ? '通过 iCloud 同步私钥？' : '停止同步私钥？'),
        content: Text(enabled
            ? '私钥与存下的口令将存入 iCloud 钥匙串，同步到登录同一 Apple 账户、开启了 iCloud 钥匙串的其他设备。'
                'iCloud 钥匙串是端到端加密的，但私钥将不再只留在这台设备上。'
            : '私钥与存下的口令会从 iCloud 钥匙串移回这台设备，其他设备上的这些私钥会随之删除。'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('取消'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: Text(enabled ? '上传并同步' : '停止同步'),
          ),
        ],
      ),
    );
    if (confirmed != true || !mounted) return;
    setState(() => _syncBusy = true);
    await _run(() => setKeySync(enabled));
    if (mounted) setState(() => _syncBusy = false);
  }

  Future<void> _rename(KeySummary key) async {
    final name = await showDialog<String>(
      context: context,
      builder: (_) => NameDialog(title: '重命名私钥', initial: key.name),
    );
    if (name != null) await _run(() => renameKey(key.id, name));
  }

  Future<void> _delete(KeySummary key) async {
    if (key.usedBy > 0) {
      await showDialog<void>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text('私钥「${key.name}」还在用'),
          content: Text('${key.usedBy} 个连接用它登录。先把这些连接换成别的私钥或认证方式，再删除它。'),
          actions: [
            TextButton(onPressed: () => Navigator.pop(context), child: const Text('知道了')),
          ],
        ),
      );
      return;
    }
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text('删除私钥「${key.name}」？'),
        content: Text(switch ((key.cardIdent.isNotEmpty, key.synchronized)) {
          (true, _) => '只删除这张卡的登记，卡上的密钥不受影响，以后可以再添加。',
          (false, true) => '私钥会从 iCloud 钥匙串删除，其他设备上也将不再有它。',
          (false, false) => '私钥与存下的口令会从钥匙串删除。',
        }),
        actions: [
          TextButton(onPressed: () => Navigator.pop(context, false), child: const Text('取消')),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            style: TextButton.styleFrom(foregroundColor: Theme.of(context).colorScheme.error),
            child: const Text('删除'),
          ),
        ],
      ),
    );
    if (confirmed == true) await _run(() => deleteKey(key.id));
  }

  void _show(KeySummary key) {
    showModalBottomSheet<void>(
      context: context,
      showDragHandle: true,
      builder: (sheetContext) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(20, 0, 20, 16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(key.name, style: Theme.of(context).textTheme.titleLarge),
              const SizedBox(height: 8),
              Text(switch ((key.cardIdent.isNotEmpty, key.encrypted, key.passphraseSaved)) {
                (true, _, _) => '${key.algorithm} · 私钥在 OpenPGP 卡（卡号 ${key.cardIdent}）上，'
                    '登录时要插着卡并输入卡的 PIN',
                (false, false, _) => key.algorithm,
                (false, true, false) => '${key.algorithm} · 有口令保护',
                (false, true, true) => '${key.algorithm} · 有口令保护，口令已存入钥匙串',
              }),
              if (key.usedBy > 0) Text('${key.usedBy} 个连接在用'),
              const SizedBox(height: 12),
              const Text('公钥（加到服务器的 ~/.ssh/authorized_keys）'),
              const SizedBox(height: 4),
              SelectableText(
                key.publicKey,
                maxLines: 4,
                style: const TextStyle(fontFamily: 'Menlo', fontSize: 11),
              ),
              const SizedBox(height: 4),
              SelectableText(
                key.fingerprint,
                style: const TextStyle(fontFamily: 'Menlo', fontSize: 11),
              ),
              const SizedBox(height: 12),
              Wrap(
                spacing: 8,
                children: [
                  FilledButton.icon(
                    onPressed: () {
                      Clipboard.setData(ClipboardData(text: key.publicKey));
                      Navigator.pop(sheetContext);
                      _snack('公钥已复制');
                    },
                    icon: const Icon(Icons.copy),
                    label: const Text('复制公钥'),
                  ),
                  OutlinedButton(
                    onPressed: () {
                      Navigator.pop(sheetContext);
                      _rename(key);
                    },
                    child: const Text('重命名'),
                  ),
                  if (key.passphraseSaved)
                    OutlinedButton(
                      onPressed: () {
                        Navigator.pop(sheetContext);
                        _run(() => forgetPassphrase(key.id));
                      },
                      child: const Text('忘记口令'),
                    ),
                  OutlinedButton(
                    onPressed: () {
                      Navigator.pop(sheetContext);
                      _delete(key);
                    },
                    style: OutlinedButton.styleFrom(
                      foregroundColor: Theme.of(context).colorScheme.error,
                    ),
                    child: const Text('删除'),
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final state = _state;
    return Scaffold(
      appBar: AppBar(
        title: const Text('私钥'),
        actions: [
          PopupMenuButton<VoidCallback>(
            tooltip: '添加',
            icon: const Icon(Icons.add),
            onSelected: (action) => action(),
            itemBuilder: (_) => [
              PopupMenuItem(value: _import, child: const Text('导入私钥')),
              PopupMenuItem(value: _addCard, child: const Text('添加 OpenPGP 卡')),
            ],
          ),
        ],
      ),
      body: state == null
          ? const Center(child: CircularProgressIndicator())
          : SafeArea(
              child: ListView(
                children: [
                  SwitchListTile(
                    title: const Text('通过 iCloud 钥匙串同步私钥'),
                    subtitle: Text(state.syncEnabled
                        ? '私钥保存在 iCloud 钥匙串，随 Apple 账户同步到其他设备'
                        : '私钥只保存在这台设备上'),
                    value: state.syncEnabled,
                    onChanged: _syncBusy ? null : _toggleSync,
                  ),
                  const Divider(),
                  if (state.keys.isEmpty)
                    Padding(
                      padding: const EdgeInsets.all(24),
                      child: Column(
                        children: [
                          const Text('还没有私钥'),
                          const SizedBox(height: 12),
                          Wrap(
                            spacing: 12,
                            runSpacing: 8,
                            alignment: WrapAlignment.center,
                            children: [
                              FilledButton.icon(
                                onPressed: _import,
                                icon: const Icon(Icons.add),
                                label: const Text('导入私钥'),
                              ),
                              OutlinedButton.icon(
                                onPressed: _addCard,
                                icon: const Icon(Icons.credit_card),
                                label: const Text('添加 OpenPGP 卡'),
                              ),
                            ],
                          ),
                        ],
                      ),
                    ),
                  for (final key in state.keys)
                    ListTile(
                      leading: Icon(key.cardIdent.isEmpty ? Icons.key : Icons.credit_card),
                      title: Text(key.name),
                      subtitle: Text(
                        key.cardIdent.isEmpty
                            ? '${key.algorithm} · ${key.fingerprint}'
                            : 'OpenPGP 卡 ${key.cardIdent} · ${key.fingerprint}',
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      trailing: key.synchronized ? const Icon(Icons.cloud_done_outlined) : null,
                      onTap: () => _show(key),
                    ),
                ],
              ),
            ),
    );
  }
}
