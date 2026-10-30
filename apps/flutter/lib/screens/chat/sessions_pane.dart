import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../ffi/hk_gate.dart' as gate;
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import '../../widgets/hk_ui.dart';
import 'chat_providers.dart';

/// Custom chat titles (`session.titles`); the log keeps the model-generated one.
class SessionTitles extends AsyncNotifier<Map<String, String>> {
  @override
  Future<Map<String, String>> build() async {
    try {
      final v = await gate.hk('session.titles');
      return v is Map ? v.map((k, v) => MapEntry('$k', '$v')) : {};
    } catch (_) {
      return {};
    }
  }

  Future<void> rename(String id, String title) async {
    await gate.hk('session.rename', {'session_id': id, 'title': title});
    state = AsyncData({...?state.value, id: title});
  }
}

final sessionTitlesProvider =
    AsyncNotifierProvider<SessionTitles, Map<String, String>>(
      SessionTitles.new,
    );

/// Display title: custom name, then the session's own title.
String sessionTitle(WidgetRef ref, SessionInfoDto s) {
  final custom = ref.watch(sessionTitlesProvider).value?[s.id];
  final t = custom ?? s.title;
  return (t == null || t.isEmpty) ? 'New chat' : t;
}

class _Query extends Notifier<String> {
  @override
  String build() => '';
  void set(String q) => state = q;
}

final _queryProvider = NotifierProvider<_Query, String>(_Query.new);

/// Chat list: new chat, search, grouped by day, rename / delete per row.
class SessionsPane extends ConsumerWidget {
  const SessionsPane({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final sessions = ref.watch(sessionListProvider);
    final selected = ref.watch(selectedSessionProvider);
    final backendError = ref.watch(backendErrorProvider);
    final q = ref.watch(_queryProvider).toLowerCase();
    return ColoredBox(
      color: hk.surface,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.fromLTRB(
              HkTokens.s3,
              HkTokens.s3,
              HkTokens.s3,
              HkTokens.s2,
            ),
            child: GradientButton(
              label: 'New chat',
              icon: Icons.add,
              expand: true,
              onPressed: backendError != null
                  ? null
                  : () => _create(context, ref),
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
            child: TextField(
              onChanged: ref.read(_queryProvider.notifier).set,
              decoration: const InputDecoration(
                hintText: 'Search chats',
                prefixIcon: Icon(Icons.search, size: 18),
                prefixIconConstraints: BoxConstraints(minWidth: 36),
              ),
            ),
          ),
          const SizedBox(height: HkTokens.s2),
          Expanded(
            child: backendError != null
                ? _Message(backendError, error: true)
                : sessions.when(
                    loading: () =>
                        const Center(child: CircularProgressIndicator()),
                    error: (e, _) => _Message(describeError(e), error: true),
                    data: (all) {
                      final list = [
                        for (final s in all)
                          if (q.isEmpty ||
                              sessionTitle(ref, s).toLowerCase().contains(q) ||
                              (s.model ?? '').toLowerCase().contains(q))
                            s,
                      ];
                      if (list.isEmpty) {
                        return _Message(
                          all.isEmpty ? 'No chats yet.' : 'No matches.',
                        );
                      }
                      final rows = <Widget>[];
                      String? group;
                      for (final s in list) {
                        final g = _group(s.updatedMs.toInt());
                        if (g != group) {
                          group = g;
                          rows.add(
                            SectionLabel(
                              g,
                              padding: const EdgeInsets.fromLTRB(
                                HkTokens.s4,
                                HkTokens.s3,
                                HkTokens.s4,
                                HkTokens.s1,
                              ),
                            ),
                          );
                        }
                        rows.add(
                          _SessionTile(
                            info: s,
                            title: sessionTitle(ref, s),
                            selected: s.id == selected,
                            onTap: () => ref
                                .read(selectedSessionProvider.notifier)
                                .select(s.id),
                          ),
                        );
                      }
                      return ListView(
                        padding: const EdgeInsets.only(bottom: HkTokens.s3),
                        children: rows,
                      );
                    },
                  ),
          ),
        ],
      ),
    );
  }

  Future<void> _create(BuildContext context, WidgetRef ref) async {
    final err = await ref.read(sessionListProvider.notifier).create();
    if (err != null && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(err)));
    }
  }
}

String _group(int ms) {
  final now = DateTime.now();
  final d = DateTime.fromMillisecondsSinceEpoch(ms);
  final days = DateTime(
    now.year,
    now.month,
    now.day,
  ).difference(DateTime(d.year, d.month, d.day)).inDays;
  if (days <= 0) return 'Today';
  if (days == 1) return 'Yesterday';
  if (days < 7) return 'Previous 7 days';
  if (days < 30) return 'Previous 30 days';
  return 'Older';
}

class _SessionTile extends ConsumerStatefulWidget {
  const _SessionTile({
    required this.info,
    required this.title,
    required this.selected,
    required this.onTap,
  });

  final SessionInfoDto info;
  final String title;
  final bool selected;
  final VoidCallback onTap;

  @override
  ConsumerState<_SessionTile> createState() => _SessionTileState();
}

class _SessionTileState extends ConsumerState<_SessionTile> {
  bool _hover = false;

  Future<void> _rename() async {
    final c = TextEditingController(text: widget.title);
    final t = await showDialog<String>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Rename chat'),
        content: SizedBox(
          width: 360,
          child: TextField(
            controller: c,
            autofocus: true,
            onSubmitted: (v) => Navigator.pop(ctx, v),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, c.text),
            child: const Text('Rename'),
          ),
        ],
      ),
    );
    if (t != null && t.trim().isNotEmpty) {
      await ref
          .read(sessionTitlesProvider.notifier)
          .rename(widget.info.id, t.trim());
    }
  }

  Future<void> _delete() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Delete chat?'),
        content: Text('"${widget.title}" and its history will be removed.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: context.hk.err),
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    try {
      await gate.hk('session.delete', {'session_id': widget.info.id});
      if (ref.read(selectedSessionProvider) == widget.info.id) {
        ref.read(selectedSessionProvider.notifier).select(null);
      }
      await ref.read(sessionListProvider.notifier).refresh();
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(describeError(e))));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final info = widget.info;
    final sel = widget.selected;
    final model = info.model?.split('/').last;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
        child: Material(
          color: sel
              ? hk.surface3
              : (_hover ? hk.surface2 : Colors.transparent),
          borderRadius: BorderRadius.circular(HkTokens.radiusControl),
          child: InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusControl),
            onTap: widget.onTap,
            onSecondaryTap: _rename,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(10, 8, 4, 8),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          widget.title,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(
                            fontSize: 13,
                            fontWeight: sel ? FontWeight.w600 : FontWeight.w500,
                          ),
                        ),
                        const SizedBox(height: 2),
                        Text(
                          [?model, _ago(info.updatedMs.toInt())].join(' · '),
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: TextStyle(fontSize: 11, color: hk.textFaint),
                        ),
                      ],
                    ),
                  ),
                  if (info.running)
                    Padding(
                      padding: const EdgeInsets.symmetric(horizontal: 6),
                      child: SizedBox(
                        width: 12,
                        height: 12,
                        child: CircularProgressIndicator(
                          strokeWidth: 2,
                          color: hk.accent,
                        ),
                      ),
                    )
                  else if (_hover || sel)
                    PopupMenuButton<String>(
                      tooltip: 'More',
                      iconSize: 16,
                      padding: EdgeInsets.zero,
                      icon: Icon(Icons.more_horiz, color: hk.textMuted),
                      onSelected: (v) => v == 'rename' ? _rename() : _delete(),
                      itemBuilder: (_) => const [
                        PopupMenuItem(
                          value: 'rename',
                          child: ListTile(
                            leading: Icon(Icons.edit_outlined, size: 18),
                            title: Text('Rename'),
                          ),
                        ),
                        PopupMenuItem(
                          value: 'delete',
                          child: ListTile(
                            leading: Icon(Icons.delete_outline, size: 18),
                            title: Text('Delete'),
                          ),
                        ),
                      ],
                    ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}

String _ago(int ms) {
  final d = DateTime.now().difference(DateTime.fromMillisecondsSinceEpoch(ms));
  if (d.inMinutes < 1) return 'now';
  if (d.inHours < 1) return '${d.inMinutes}m ago';
  if (d.inDays < 1) return '${d.inHours}h ago';
  return '${d.inDays}d ago';
}

class _Message extends StatelessWidget {
  const _Message(this.text, {this.error = false});

  final String text;
  final bool error;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(HkTokens.s4),
        child: Text(
          text,
          textAlign: TextAlign.center,
          style: TextStyle(color: error ? hk.err : hk.textMuted, fontSize: 13),
        ),
      ),
    );
  }
}
