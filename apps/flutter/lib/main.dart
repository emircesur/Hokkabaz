import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'app/router.dart';
import 'theme/theme.dart';

void main() {
  runApp(const ProviderScope(child: HokkabazApp()));
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
