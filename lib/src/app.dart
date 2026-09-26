import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'bindings/bindings.dart';
import 'catalog/connection_list_page.dart';
import 'terminal/session_target.dart';
import 'workspace/workspace_page.dart';

/// 调试用的自动连接（debug 构建）：启动后直接以快速连接打开终端。取值先看 --dart-define
/// （见 README），没有就看进程环境变量（XCUITest 经 launchEnvironment 传入，M6）。
String _autoValue(String name, String defined) =>
    defined.isNotEmpty ? defined : (Platform.environment[name] ?? '');

final _autoHost = _autoValue('GUOSH_HOST', const String.fromEnvironment('GUOSH_HOST'));
final _autoPort = int.tryParse(_autoValue('GUOSH_PORT', const String.fromEnvironment('GUOSH_PORT'))) ?? 22;
final _autoUser = _autoValue('GUOSH_USER', const String.fromEnvironment('GUOSH_USER'));
final _autoPass = _autoValue('GUOSH_PASS', const String.fromEnvironment('GUOSH_PASS'));
final _autoCmd = _autoValue('GUOSH_CMD', const String.fromEnvironment('GUOSH_CMD'));

class GuoSSHellApp extends StatelessWidget {
  const GuoSSHellApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'GuoSSHell',
      theme: ThemeData(
        brightness: Brightness.dark,
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.teal,
          brightness: Brightness.dark,
        ),
      ),
      home: const _StartupGate(),
    );
  }
}

/// 启动：把数据目录交给 Rust 打开存储，等它就绪、设置到位后进入连接列表。
class _StartupGate extends StatefulWidget {
  const _StartupGate();

  @override
  State<_StartupGate> createState() => _StartupGateState();
}

class _StartupGateState extends State<_StartupGate> {
  StreamSubscription? _readySub;
  StreamSubscription? _settingsSub;
  bool _ready = false;
  String? _error;

  /// 前后台切换告诉 Rust（进后台时它向系统申请一小段后台时间，连接不会立刻挂起）。
  late final AppLifecycleListener _lifecycle;

  @override
  void initState() {
    super.initState();
    _lifecycle = AppLifecycleListener(
      onShow: () => AppLifecycle(foreground: true).sendSignalToRust(),
      onHide: () => AppLifecycle(foreground: false).sendSignalToRust(),
    );
    _readySub = AppReady.rustSignalStream.listen((pack) {
      if (!mounted) return;
      if (pack.message.ok) {
        SettingsQuery().sendSignalToRust();
      } else {
        setState(() => _error = pack.message.detail);
      }
    });
    // 终端页的字体取自设置：第一份设置到了才算就绪。
    _settingsSub = SettingsState.rustSignalStream.listen((_) {
      if (!mounted || _ready) return;
      setState(() => _ready = true);
      _autoConnect();
    });
    _start();
  }

  Future<void> _start() async {
    try {
      final directory = await getApplicationSupportDirectory();
      AppStart(supportDir: directory.path).sendSignalToRust();
    } catch (error) {
      if (mounted) setState(() => _error = '$error');
    }
  }

  void _autoConnect() {
    if (!kDebugMode || _autoHost.isEmpty) return;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      Navigator.of(context).push(MaterialPageRoute<void>(
        builder: (_) => WorkspacePage(
          initial: SessionTarget.quick(
            host: _autoHost,
            port: _autoPort,
            username: _autoUser,
            password: _autoPass,
            command: _autoCmd,
          ),
        ),
      ));
    });
  }

  @override
  void dispose() {
    _lifecycle.dispose();
    _readySub?.cancel();
    _settingsSub?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final error = _error;
    if (error != null) {
      return Scaffold(
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Text('无法打开数据存储：$error', textAlign: TextAlign.center),
          ),
        ),
      );
    }
    if (!_ready) {
      return const Scaffold(body: Center(child: CircularProgressIndicator()));
    }
    return const ConnectionListPage();
  }
}
