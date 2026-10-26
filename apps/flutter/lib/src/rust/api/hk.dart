// Hand-written to match `crates/hk-ffi/src/api/hk.rs` (same shape flutter_rust_bridge
// generates); regenerate with `flutter_rust_bridge_codegen generate` when the toolchain is
// available.

// ignore_for_file: invalid_use_of_internal_member, unused_import, unnecessary_import

import '../frb_generated.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';

/// Run `method` (e.g. `jobs.list`, `memory.save`) with JSON `paramsJson`; returns JSON.
Future<String> hkCall({required String method, required String paramsJson}) =>
    RustLib.instance.api.crateApiHkHkCall(method: method, paramsJson: paramsJson);

/// Stream of JSON events for `topic` (`jobs`).
Stream<String> hkEvents({required String topic}) =>
    RustLib.instance.api.crateApiHkHkEvents(topic: topic);
