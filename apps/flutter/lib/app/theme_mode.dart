import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:path_provider/path_provider.dart';

/// Theme choice, persisted as a one-word file in the app support dir. A file
/// avoids a native prefs plugin; any I/O failure just falls back to dark.
class ThemeModeController extends Notifier<ThemeMode> {
  bool _touched = false;

  @override
  ThemeMode build() {
    unawaited(_load());
    // Dark-first (DESIGN §4.7).
    return ThemeMode.dark;
  }

  Future<File?> _file() async {
    try {
      final dir = await getApplicationSupportDirectory();
      return File('${dir.path}${Platform.pathSeparator}theme_mode');
    } catch (_) {
      return null;
    }
  }

  Future<void> _load() async {
    try {
      final f = await _file();
      if (f == null || !await f.exists()) return;
      final name = (await f.readAsString()).trim();
      final m = ThemeMode.values.where((m) => m.name == name).firstOrNull;
      // A choice made while loading wins over the stored one.
      if (m != null && !_touched) state = m;
    } catch (_) {
      // Keep the default.
    }
  }

  Future<void> set(ThemeMode m) async {
    _touched = true;
    state = m;
    try {
      final f = await _file();
      await f?.writeAsString(m.name);
    } catch (_) {
      // Not persisted this time; the in-memory choice still applies.
    }
  }

  /// dark -> light -> system -> dark.
  Future<void> cycle() => set(switch (state) {
    ThemeMode.dark => ThemeMode.light,
    ThemeMode.light => ThemeMode.system,
    ThemeMode.system => ThemeMode.dark,
  });
}

final themeModeProvider = NotifierProvider<ThemeModeController, ThemeMode>(
  ThemeModeController.new,
);

String themeModeLabel(ThemeMode m) => switch (m) {
  ThemeMode.dark => 'Dark',
  ThemeMode.light => 'Light',
  ThemeMode.system => 'System',
};

IconData themeModeIcon(ThemeMode m) => switch (m) {
  ThemeMode.dark => Icons.dark_mode_outlined,
  ThemeMode.light => Icons.light_mode_outlined,
  ThemeMode.system => Icons.brightness_auto_outlined,
};
