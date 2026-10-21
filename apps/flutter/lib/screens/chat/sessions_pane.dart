import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';

/// Session list with "new session"; selection drives the conversation pane.
class SessionsPane extends ConsumerWidget {
  const SessionsPane({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final sessions = ref.watch(sessionListProvider);
    final selected = ref.watch(selectedSessionProvider);
    final backendError = ref.watch(backendErrorProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            HkTokens.s4,
            HkTokens.s3,
            HkTokens.s2,
            HkTokens.s2,
          ),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  'Sessions',
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
              IconButton(
                tooltip: 'New session',
                icon: const Icon(Icons.add),
                onPressed: backendError != null
                    ? null
                    : () => _create(context, ref),
              ),
            ],
          ),
        ),
        Divider(height: 1, color: context.hk.border),
        Expanded(
          child: backendError != null
              ? _Message(backendError, error: true)
              : sessions.when(
                  loading: () =>
                      const Center(child: CircularProgressIndicator()),
                  error: (e, _) => _Message(describeError(e), error: true),
                  data: (list) => list.isEmpty
                      ? const _Message('No sessions yet.')
                      : ListView.builder(
                          itemCount: list.length,
                          itemBuilder: (context, i) => _SessionTile(
                            info: list[i],
                            selected: list[i].id == selected,
                            onTap: () => ref
                                .read(selectedSessionProvider.notifier)
                                .select(list[i].id),
                          ),
                        ),
                ),
        ),
      ],
    );
  }

  Future<void> _create(BuildContext context, WidgetRef ref) async {
    final err = await ref.read(sessionListProvider.notifier).create();
    if (err != null && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(err)));
    }
  }
}

class _SessionTile extends StatelessWidget {
  const _SessionTile({
    required this.info,
    required this.selected,
    required this.onTap,
  });

  final SessionInfoDto info;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final title = (info.title == null || info.title!.isEmpty)
        ? 'New session'
        : info.title!;
    return ListTile(
      dense: true,
      selected: selected,
      selectedTileColor: hk.surface,
      selectedColor: hk.text,
      onTap: onTap,
      title: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: Text(
        [
          if (info.model != null) info.model!,
          _ago(info.updatedMs.toInt()),
        ].join(' · '),
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: info.running
          ? SizedBox(
              width: 12,
              height: 12,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: hk.accent,
              ),
            )
          : null,
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
          style: TextStyle(color: error ? hk.err : hk.textMuted),
        ),
      ),
    );
  }
}
