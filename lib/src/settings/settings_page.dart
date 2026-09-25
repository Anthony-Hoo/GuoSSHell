import 'dart:async';

import 'package:flutter/material.dart';

import '../bindings/bindings.dart';
import 'terminal_font.dart';

/// 设置：终端字体与字号（存在 Rust 侧的默认终端配置里，新会话生效）。
class SettingsPage extends StatefulWidget {
  const SettingsPage({super.key});

  @override
  State<SettingsPage> createState() => _SettingsPageState();
}

class _SettingsPageState extends State<SettingsPage> {
  StreamSubscription? _settingsSub;
  SettingsState? _settings = SettingsState.latestRustSignal?.message;

  /// 拖动中的字号（松手才保存）。
  double? _draggingSize;

  @override
  void initState() {
    super.initState();
    _settingsSub = SettingsState.rustSignalStream.listen((pack) {
      if (mounted) setState(() => _settings = pack.message);
    });
    SettingsQuery().sendSignalToRust();
  }

  @override
  void dispose() {
    _settingsSub?.cancel();
    super.dispose();
  }

  void _save({String? fontFamily, double? fontSize}) {
    final settings = _settings;
    if (settings == null) return;
    SaveSettings(
      fontFamily: fontFamily ?? settings.fontFamily,
      fontSize: fontSize ?? settings.fontSize,
    ).sendSignalToRust();
  }

  @override
  Widget build(BuildContext context) {
    final settings = _settings;
    return Scaffold(
      appBar: AppBar(title: const Text('设置')),
      body: settings == null
          ? const Center(child: CircularProgressIndicator())
          : _buildBody(context, settings),
    );
  }

  Widget _buildBody(BuildContext context, SettingsState settings) {
    final scheme = Theme.of(context).colorScheme;
    final size = _draggingSize ?? settings.fontSize;
    return SafeArea(
      child: ListView(
        padding: const EdgeInsets.symmetric(vertical: 8),
        children: [
          const _SectionTitle('终端字体'),
          RadioGroup<String>(
            groupValue: settings.fontFamily,
            onChanged: (family) {
              if (family != null) _save(fontFamily: family);
            },
            child: Column(
              children: [
                for (final family in settings.fontFamilies)
                  RadioListTile<String>(
                    value: family,
                    title: Text(family, style: TextStyle(fontFamily: family)),
                    subtitle: family == bundledFontFamily
                        ? const Text('内置，含 powerline 与图标字形')
                        : const Text('系统字体'),
                  ),
              ],
            ),
          ),
          const _SectionTitle('字号'),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Row(
              children: [
                Expanded(
                  child: Slider(
                    min: settings.minFontSize,
                    max: settings.maxFontSize,
                    divisions: (settings.maxFontSize - settings.minFontSize).round(),
                    value: size.clamp(settings.minFontSize, settings.maxFontSize),
                    label: size.round().toString(),
                    onChanged: (value) => setState(() => _draggingSize = value),
                    onChangeEnd: (value) {
                      setState(() => _draggingSize = null);
                      _save(fontSize: value.roundToDouble());
                    },
                  ),
                ),
                SizedBox(width: 32, child: Text('${size.round()}')),
              ],
            ),
          ),
          Container(
            margin: const EdgeInsets.all(16),
            padding: const EdgeInsets.all(12),
            color: Colors.black,
            child: Text(
              'probe@nas:~\$ ls -la\n main  ✔  中文 ⌘',
              style: terminalStyle(settings).toTextStyle().copyWith(
                    fontSize: size,
                    color: Colors.white,
                  ),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16),
            child: Text(
              '新打开的会话使用新设置。',
              style: TextStyle(color: scheme.onSurfaceVariant, fontSize: 13),
            ),
          ),
        ],
      ),
    );
  }
}

class _SectionTitle extends StatelessWidget {
  final String text;
  const _SectionTitle(this.text);

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 16, 16, 4),
      child: Text(
        text,
        style: Theme.of(context).textTheme.titleSmall?.copyWith(
              color: Theme.of(context).colorScheme.primary,
            ),
      ),
    );
  }
}
