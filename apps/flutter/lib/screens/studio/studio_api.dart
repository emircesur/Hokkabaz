import 'dart:convert';

import '../../src/rust/api/studio.dart' as native;

typedef Json = Map<String, dynamic>;

Json object(dynamic value) => Map<String, dynamic>.from(value as Map);
List<Json> objects(dynamic value) => (value as List).map(object).toList();

Future<dynamic> studioRequest(String operation, [Json args = const {}]) async {
  final result = await native.studioRequest(
    operation: operation,
    argsJson: jsonEncode(args),
  );
  return jsonDecode(result);
}

String timeLabel(num seconds) {
  final value = seconds.floor();
  return '${value ~/ 60}:${(value % 60).toString().padLeft(2, '0')}';
}
