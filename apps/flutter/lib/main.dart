import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app/router.dart';
import 'ffi/ffi.dart';
import 'theme/theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final rustError = await initRust();
  final coreError = await initCore(rustError);
  runApp(
    ProviderScope(
      overrides: [
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
      // Dark-first (DESIGN §4.7); user override comes with Settings.
      themeMode: ThemeMode.dark,
      routerConfig: ref.watch(routerProvider),
    );
  }
}
