import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/core.dart' as core;
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

/// Rust -> Dart stream demo (0..4, 500 ms apart).
final demoTickerProvider = StreamProvider<int>((ref) {
  if (ref.watch(rustInitErrorProvider) != null) return const Stream.empty();
  return core.demoTicker(count: 5, intervalMs: 500);
});
