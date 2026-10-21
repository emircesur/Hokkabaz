import 'chat_model.dart';

/// Inspector view-model: everything is derived from the chat items (the
/// session feed has no dedicated todo/plan/subagent/cost events), so it also
/// works for history loaded via `get_session`. Pure Dart, no widgets.

enum TodoStatus { pending, inProgress, completed }

class TodoEntry {
  const TodoEntry(this.content, this.status);
  final String content;
  final TodoStatus status;
}

/// Latest `todo` call wins (whole-list replace per call). Empty if none.
List<TodoEntry> todosFromItems(List<ChatItem> items) {
  for (var i = items.length - 1; i >= 0; i--) {
    final it = items[i];
    if (it is! ToolItem || it.name != 'todo') continue;
    final raw = it.args?['todos'];
    if (raw is! List) continue; // still streaming / malformed: try older one
    return [
      for (final t in raw)
        if (t is Map && t['content'] is String)
          TodoEntry(t['content'] as String, switch (t['status']) {
            'completed' => TodoStatus.completed,
            'in_progress' => TodoStatus.inProgress,
            _ => TodoStatus.pending,
          }),
    ];
  }
  return const [];
}

/// One `task` tool call = one subagent run in its own child session.
class SubagentRun {
  const SubagentRun({
    required this.callId,
    required this.title,
    required this.mode,
    required this.status,
    this.sessionId,
  });

  final String callId;
  final String title;
  final String mode;
  final ToolStatus status;

  /// Parsed from the result footer `[subagent session <id>]`; null while running.
  final String? sessionId;
}

final _childRe = RegExp(r'\[subagent session ([^\]\s]+)\]');

List<SubagentRun> subagentsFromItems(List<ChatItem> items) => [
  for (final it in items)
    if (it is ToolItem && it.name == 'task')
      SubagentRun(
        callId: it.callId,
        title: _taskTitle(it.args),
        mode: (it.args?['mode'] as String?) ?? 'build',
        status: it.status,
        sessionId: it.result == null
            ? null
            : _childRe.firstMatch(it.result!)?.group(1),
      ),
];

String _taskTitle(Map<String, dynamic>? a) {
  final d = a?['description'];
  if (d is String && d.isNotEmpty) return d;
  final p = a?['prompt'];
  if (p is String && p.isNotEmpty) {
    final line = p.split('\n').first;
    return line.length > 60 ? '${line.substring(0, 60)}…' : line;
  }
  return 'subagent';
}

/// A file touched by `edit` / `write` calls, newest change last.
class ChangedFile {
  const ChangedFile({
    required this.path,
    required this.edits,
    required this.added,
    required this.removed,
    required this.diff,
  });

  final String path;
  final int edits;
  final int added;
  final int removed;

  /// All of the file's diffs concatenated, in call order.
  final List<DiffLine> diff;
}

List<ChangedFile> changedFilesFromItems(List<ChatItem> items) {
  final byPath = <String, List<List<DiffLine>>>{};
  for (final it in items) {
    if (it is! ToolItem) continue;
    if (it.status == ToolStatus.error || it.status == ToolStatus.denied) {
      continue;
    }
    final a = it.args;
    final d = diffForTool(it.name, a);
    if (a == null || d == null) continue;
    final path = (a['path'] ?? a['file_path']);
    if (path is! String || path.isEmpty) continue;
    byPath.putIfAbsent(path, () => []).add(d);
  }
  return [
    for (final e in byPath.entries)
      ChangedFile(
        path: e.key,
        edits: e.value.length,
        added: e.value
            .expand((d) => d)
            .where((l) => l.kind == DiffKind.add)
            .length,
        removed: e.value
            .expand((d) => d)
            .where((l) => l.kind == DiffKind.remove)
            .length,
        diff: [for (final d in e.value) ...d],
      ),
  ];
}
