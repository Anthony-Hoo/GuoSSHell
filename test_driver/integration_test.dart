// flutter drive 的驱动端（M6）：截图落盘到 build/m6/screenshots/<设备>/，集成测试写的
// reportData 落盘到 build/m6/reports/<M6_REPORT>.json（scripts/m6.sh 设 M6_REPORT）。
import 'dart:convert';
import 'dart:io';

import 'package:integration_test/integration_test_driver_extended.dart';

Future<void> main() async {
  final report = Platform.environment['M6_REPORT'] ?? 'report';
  await integrationDriver(
    writeResponseOnFailure: true,
    onScreenshot: (name, bytes, [args]) async {
      final file = File('build/m6/screenshots/$name.png');
      await file.parent.create(recursive: true);
      await file.writeAsBytes(bytes);
      return true;
    },
    responseDataCallback: (data) async {
      final file = File('build/m6/reports/$report.json');
      await file.parent.create(recursive: true);
      await file.writeAsString(const JsonEncoder.withIndent('  ').convert(data));
    },
  );
}
