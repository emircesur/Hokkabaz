import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app/router.dart';
import 'app/theme_mode.dart';
import 'ffi/android_runtime.dart';
import 'ffi/ffi.dart';
import 'ffi/notifications.dart';
import 'theme/theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final rustError = await initRust();
  final coreError = await initCore(rustError);
  await requestNotificationPermission();
  runApp(
    ProviderScope(
      overrides: [
        if (Platform.isAndroid) approvalNotifierProvider.overrideWithValue(const AndroidApprovalNotifier()),
        rustInitErrorProvider.overrideWithValue(rustError),
        coreInitErrorProvider.overrideWithValue(coreError),
      ],
      child: const HokkabazApp(),
    ),
  );
}

class HokkabazApp extends ConsumerWidget {
  const HokkabazApp({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return MaterialApp.router(
      title: 'Hokkabaz',
      debugShowCheckedModeBanner: false,
      theme: buildTheme(Brightness.light),
      darkTheme: buildTheme(Brightness.dark),
      themeMode: ref.watch(themeModeProvider),
      routerConfig: ref.watch(routerProvider),
    );
  }
}
