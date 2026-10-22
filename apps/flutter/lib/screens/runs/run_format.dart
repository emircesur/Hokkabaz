import 'dart:convert';

/// `2026-10-22 14:05:09` in local time.
String formatTime(int ms) {
  if (ms <= 0) return 'unknown time';
  final t = DateTime.fromMillisecondsSinceEpoch(ms);
  String p(int n) => n.toString().padLeft(2, '0');
  return '${t.year}-${p(t.month)}-${p(t.day)} ${p(t.hour)}:${p(t.minute)}:${p(t.second)}';
}

/// `just now`, `5 min ago`, `3 h ago`, `2 d ago`; older than a week falls back to a date.
String relativeTime(int ms, {int? nowMs}) {
  if (ms <= 0) return 'unknown time';
  final diff = (nowMs ?? DateTime.now().millisecondsSinceEpoch) - ms;
  if (diff < 45 * 1000) return 'just now';
  if (diff < 60 * 60 * 1000) return '${(diff / 60000).round().clamp(1, 59)} min ago';
  if (diff < 24 * 3600 * 1000) return '${diff ~/ 3600000} h ago';
  if (diff < 7 * 24 * 3600 * 1000) return '${diff ~/ 86400000} d ago';
  return formatTime(ms).substring(0, 10);
}

/// Local flow file a run belongs to. Local runs carry the file id as the workflow
/// `id`; the instance's copy of an app workflow keeps it in `meta.hokkabaz.localId`.
String? localFlowIdOf(String? workflowJson) {
  if (workflowJson == null) return null;
  try {
    final j = jsonDecode(workflowJson);
    if (j is! Map) return null;
    final meta = j['meta'];
    if (meta is Map) {
      final h = meta['hokkabaz'];
      if (h is Map && h['localId'] is String) return h['localId'] as String;
    }
    final id = j['id'];
    return id is String && id.isNotEmpty ? id : null;
  } on FormatException {
    return null;
  }
}
