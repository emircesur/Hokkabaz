import 'dart:convert';

import '../src/rust/api/hk.dart' as gate;

/// Call a `domain.action` command on the core (`hk_call`); returns decoded JSON.
Future<dynamic> hk(
  String method, [
  Map<String, dynamic> params = const {},
]) async {
  final raw = await gate.hkCall(method: method, paramsJson: jsonEncode(params));
  if (raw.isEmpty) return null;
  return jsonDecode(raw);
}

/// Typed convenience: result as a list of maps (empty when not a list).
Future<List<Map<String, dynamic>>> hkList(
  String method, [
  Map<String, dynamic> params = const {},
]) async {
  final v = await hk(method, params);
  if (v is! List) return const [];
  return [
    for (final e in v)
      if (e is Map) e.cast<String, dynamic>(),
  ];
}

/// Typed convenience: result as a map (empty when not a map).
Future<Map<String, dynamic>> hkMap(
  String method, [
  Map<String, dynamic> params = const {},
]) async {
  final v = await hk(method, params);
  return v is Map ? v.cast<String, dynamic>() : <String, dynamic>{};
}

/// Event feed (`jobs`, …) as decoded JSON objects.
Stream<Map<String, dynamic>> hkStream(String topic) => gate
    .hkEvents(topic: topic)
    .map((s) => jsonDecode(s))
    .where((v) => v is Map)
    .map((v) => (v as Map).cast<String, dynamic>());
