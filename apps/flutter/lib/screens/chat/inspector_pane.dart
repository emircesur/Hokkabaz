import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_model.dart';
import 'chat_providers.dart';
import 'diff_view.dart';
import 'inspector_model.dart';

/// Wide layouts show the inspector as a third column when this is true.
class InspectorOpen extends Notifier<bool> {
  @override
  bool build() => true;

  void toggle() => state = !state;
}

final inspectorOpenProvider = NotifierProvider<InspectorOpen, bool>(
  InspectorOpen.new,
);

/// Narrow layouts: same content in a bottom sheet.
Future<void> showInspectorSheet(BuildContext context, String sessionId) {
  return showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    builder: (_) => DraggableScrollableSheet(
      expand: false,
      initialChildSize: 0.7,
      maxChildSize: 0.95,
      builder: (_, controller) => InspectorPane(
        sessionId: sessionId,
        scrollController: controller,
        onClose: () => Navigator.of(context).pop(),
      ),
    ),
  );
}

/// Context / todo / cost / subagents / files-changed panels for one session.
class InspectorPane extends ConsumerWidget {
  const InspectorPane({
    super.key,
    required this.sessionId,
    this.scrollController,
    this.onClose,
  });

  final String sessionId;
  final ScrollController? scrollController;
  final VoidCallback? onClose;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final state = ref.watch(chatControllerProvider(sessionId));
    final model = ref
        .watch(sessionListProvider)
        .value
        ?.where((s) => s.id == sessionId)
        .firstOrNull
        ?.model;
    final mode = ref.watch(chatModeProvider);
    final todos = todosFromItems(state.items);
    final subs = subagentsFromItems(state.items);
    final files = changedFilesFromItems(state.items);
    final done = todos.where((t) => t.status == TodoStatus.completed).length;
    return Container(
      color: hk.bg,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(
              HkTokens.s4,
              HkTokens.s2,
              HkTokens.s2,
              HkTokens.s2,
            ),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    'Inspector',
                    style: Theme.of(context).textTheme.titleMedium,
                  ),
                ),
                if (onClose != null)
                  IconButton(
                    tooltip: 'Close inspector',
                    icon: const Icon(Icons.close, size: 18),
                    onPressed: onClose,
                  ),
              ],
            ),
          ),
          Divider(height: 1, color: hk.border),
          Expanded(
            child: ListView(
              controller: scrollController,
              padding: const EdgeInsets.all(HkTokens.s3),
              children: [
                _Section(
                  title: 'Context',
                  child: Column(
                    children: [
                      _kv(context, 'Model', model ?? 'default'),
                      _kv(
                        context,
                        'Mode',
                        mode == ModeDto.plan ? 'plan' : 'build',
                      ),
                      _kv(context, 'Context size', _n(state.lastInputTokens)),
                    ],
                  ),
                ),
                _Section(
                  title: 'Plan',
                  trailing: todos.isEmpty ? null : '$done/${todos.length}',
                  child: todos.isEmpty
                      ? _empty(context, 'No todo list yet.')
                      : Column(
                          children: [for (final t in todos) _TodoRow(entry: t)],
                        ),
                ),
                _Section(
                  title: 'Cost',
                  child: Column(
                    children: [
                      _kv(context, 'Requests', '${state.usageSteps}'),
                      _kv(context, 'Input', _n(state.inputTokens)),
                      _kv(context, 'Output', _n(state.outputTokens)),
                      _kv(context, 'Cache read', _n(state.cacheReadTokens)),
                      _kv(context, 'Reasoning', _n(state.reasoningTokens)),
                      _kv(context, 'Price', 'not reported'),
                    ],
                  ),
                ),
                _Section(
                  title: 'Subagents',
                  trailing: subs.isEmpty ? null : '${subs.length}',
                  child: subs.isEmpty
                      ? _empty(context, 'No subagents spawned.')
                      : Column(
                          children: [for (final r in subs) _SubagentRow(r)],
                        ),
                ),
                _Section(
                  title: 'Files changed',
                  trailing: files.isEmpty ? null : '${files.length}',
                  child: files.isEmpty
                      ? _empty(context, 'No edits yet.')
                      : Column(children: [for (final f in files) _FileRow(f)]),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}

String _n(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(1)}k' : '$n';

Widget _empty(BuildContext context, String text) => Padding(
  padding: const EdgeInsets.symmetric(vertical: HkTokens.s1),
  child: Text(
    text,
    style: TextStyle(fontSize: 12, color: context.hk.textMuted),
  ),
);

Widget _kv(BuildContext context, String k, String v) => Padding(
  padding: const EdgeInsets.symmetric(vertical: 2),
  child: Row(
    children: [
      Text(k, style: TextStyle(fontSize: 12, color: context.hk.textMuted)),
      const SizedBox(width: HkTokens.s2),
      Expanded(
        child: Text(
          v,
          textAlign: TextAlign.end,
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
          style: const TextStyle(fontSize: 12, fontFamily: HkTokens.fontMono),
        ),
      ),
    ],
  ),
);

class _Section extends StatelessWidget {
  const _Section({required this.title, required this.child, this.trailing});

  final String title;
  final String? trailing;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Padding(
      padding: const EdgeInsets.only(bottom: HkTokens.s3),
      child: Container(
        padding: const EdgeInsets.all(HkTokens.s3),
        decoration: BoxDecoration(
          color: hk.surface,
          border: Border.all(color: hk.border),
          borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            Row(
              children: [
                Text(
                  title,
                  style: const TextStyle(
                    fontSize: 13,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                const Spacer(),
                if (trailing != null)
                  Text(
                    trailing!,
                    style: TextStyle(fontSize: 12, color: hk.textMuted),
                  ),
              ],
            ),
            const SizedBox(height: HkTokens.s2),
            child,
          ],
        ),
      ),
    );
  }
}

class _TodoRow extends StatelessWidget {
  const _TodoRow({required this.entry});

  final TodoEntry entry;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final (icon, color) = switch (entry.status) {
      TodoStatus.completed => (Icons.check_circle, hk.ok),
      TodoStatus.inProgress => (Icons.timelapse, hk.accent),
      TodoStatus.pending => (Icons.radio_button_unchecked, hk.textMuted),
    };
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(icon, size: 14, color: color),
          const SizedBox(width: HkTokens.s2),
          Expanded(
            child: Text(
              entry.content,
              style: TextStyle(
                fontSize: 12,
                color: entry.status == TodoStatus.completed
                    ? hk.textMuted
                    : hk.text,
                decoration: entry.status == TodoStatus.completed
                    ? TextDecoration.lineThrough
                    : null,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class _SubagentRow extends ConsumerWidget {
  const _SubagentRow(this.run);

  final SubagentRun run;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final (icon, color) = switch (run.status) {
      ToolStatus.done => (Icons.check_circle_outline, hk.ok),
      ToolStatus.error || ToolStatus.denied => (Icons.error_outline, hk.err),
      _ => (Icons.timelapse, hk.accent),
    };
    final id = run.sessionId;
    return InkWell(
      // Child sessions are ordinary sessions; open it like any other.
      onTap: id == null
          ? null
          : () => ref.read(selectedSessionProvider.notifier).select(id),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 3),
        child: Row(
          children: [
            Icon(icon, size: 14, color: color),
            const SizedBox(width: HkTokens.s2),
            Expanded(
              child: Text(
                run.title,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(fontSize: 12),
              ),
            ),
            Text(run.mode, style: TextStyle(fontSize: 11, color: hk.textMuted)),
            if (id != null) ...[
              const SizedBox(width: HkTokens.s1),
              Icon(Icons.open_in_new, size: 12, color: hk.info),
            ],
          ],
        ),
      ),
    );
  }
}

class _FileRow extends StatelessWidget {
  const _FileRow(this.file);

  final ChangedFile file;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return InkWell(
      onTap: () => showDialog<void>(
        context: context,
        builder: (_) => Dialog(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 720, maxHeight: 560),
            child: Padding(
              padding: const EdgeInsets.all(HkTokens.s3),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Text(
                    file.path,
                    style: const TextStyle(
                      fontFamily: HkTokens.fontMono,
                      fontSize: 13,
                    ),
                  ),
                  const SizedBox(height: HkTokens.s2),
                  Expanded(
                    child: SingleChildScrollView(
                      child: DiffView(lines: file.diff, maxLines: 1000),
                    ),
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 3),
        child: Row(
          children: [
            Expanded(
              child: Text(
                file.path,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  fontFamily: HkTokens.fontMono,
                  fontSize: 12,
                ),
              ),
            ),
            const SizedBox(width: HkTokens.s2),
            Text(
              '+${file.added}',
              style: TextStyle(fontSize: 12, color: hk.ok),
            ),
            const SizedBox(width: HkTokens.s1),
            Text(
              '-${file.removed}',
              style: TextStyle(fontSize: 12, color: hk.err),
            ),
          ],
        ),
      ),
    );
  }
}
