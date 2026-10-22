import 'dart:io' show Platform;

import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:path_provider/path_provider.dart';

import '../src/rust/api/core.dart' as core;
import '../src/rust/api/instances.dart' as instances;
import '../src/rust/dto.dart' show CoreProfile, InstanceKindDto;
import '../src/rust/frb_generated.dart';

/// On Android the secrets file is encrypted with a key wrapped by the Android Keystore. If the
/// keystore is unavailable the core falls back to its plain 0600 file instead of failing.
Future<void> _installMasterKey() async {
  if (!Platform.isAndroid) return;
  try {
    final key = await const MethodChannel('hokkabaz/keystore').invokeMethod<Uint8List>('dataKey');
    if (key != null) core.setSecretMasterKey(key: key);
  } catch (_) {
    // Fall through: plaintext 0600 file in the app-private dir.
  }
}

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
    await _installMasterKey();
    await core.coreInit(
      dataDir: dir.path,
      profile: (Platform.isAndroid || Platform.isIOS)
          ? CoreProfile.mobile
          : CoreProfile.desktop,
    );
    await _restoreInstance();
    return null;
  } catch (e) {
    return e.toString();
  }
}

/// Reconnects the instance chosen as startup default. Best effort: an
/// unreachable remote leaves the local core active (the Instances screen shows
/// why), never a dead app.
Future<void> _restoreInstance() async {
  try {
    final book = await instances.listInstances();
    for (final i in book) {
      if (i.isDefault && i.kind == InstanceKindDto.remote) {
        await instances.connectInstance(id: i.id);
        return;
      }
    }
  } catch (_) {
    // Stay on the local core.
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
