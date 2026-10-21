import 'dart:io' show Platform;

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:path_provider/path_provider.dart';

import '../src/rust/api/core.dart' as core;
import '../src/rust/dto.dart' show CoreProfile;
import '../src/rust/frb_generated.dart';

/// Loads the Rust lib once at startup. Failure is non-fatal: the UI shows the
/// error instead of crashing, so a missing native lib never blocks the shell.
Future<String?> initRust() async {
  try {
    await RustLib.init();
    return null;
  } catch (e) {
    return e.toString();
  }
}

/// Starts the in-process core (sessions, tools, providers) rooted in the app
/// data dir. Non-fatal like [initRust]: returns the error text, null = ready.
/// Skipped when the lib itself failed to load.
Future<String?> initCore(String? rustError) async {
  if (rustError != null) return null;
  try {
    final dir = await getApplicationSupportDirectory();
    await core.coreInit(
      dataDir: dir.path,
      profile: (Platform.isAndroid || Platform.isIOS)
          ? CoreProfile.mobile
          : CoreProfile.desktop,
    );
    return null;
  } catch (e) {
    return e.toString();
  }
}

/// Error from [initCore], overridden in `main`. Null = core is up.
final coreInitErrorProvider = Provider<String?>((ref) => null);

/// First startup failure (lib load, then core init), or null when usable.
final backendErrorProvider = Provider<String?>((ref) {
  final rust = ref.watch(rustInitErrorProvider);
  if (rust != null) return 'Rust lib unavailable: $rust';
  final core = ref.watch(coreInitErrorProvider);
  if (core != null) return 'Core failed to start: $core';
  return null;
});

/// Error from [initRust], overridden in `main`. Null = Rust is up.
final rustInitErrorProvider = Provider<String?>((ref) => null);

/// `core_version()` from Rust, or the reason it is unavailable.
final coreVersionProvider = Provider<String>((ref) {
  final initError = ref.watch(rustInitErrorProvider);
  if (initError != null) return 'unavailable ($initError)';
  try {
    return core.coreVersion();
  } catch (e) {
    return 'unavailable ($e)';
  }
});
