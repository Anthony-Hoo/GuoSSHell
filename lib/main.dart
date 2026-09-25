import 'package:flutter/material.dart';
import 'package:rinf/rinf.dart';

import 'src/app.dart';
import 'src/bindings/bindings.dart';

Future<void> main() async {
  await initializeRust(assignRustSignal);
  runApp(const GuoSSHellApp());
}
