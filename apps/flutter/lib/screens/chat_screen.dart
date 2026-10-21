import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../ffi/ffi.dart';
import '../theme/tokens.dart';
import 'chat/chat_providers.dart';
import 'chat/conversation_pane.dart';
import 'chat/sessions_pane.dart';

/// Chat tab: sessions list + conversation. Two panes on wide windows, list ->
/// conversation navigation on narrow ones (DESIGN §4.3).
class ChatScreen extends ConsumerWidget {
  const ChatScreen({super.key, required this.title, required this.icon});

  final String title;
  final IconData icon;

  /// Content width at which sessions and conversation sit side by side.
  static const double _twoPaneMin = 720;
  static const double _sessionsWidth = 280;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final selected = ref.watch(selectedSessionProvider);
    return LayoutBuilder(
      builder: (context, c) {
        final wide = c.maxWidth >= _twoPaneMin;
        final conversation = selected == null
            ? _Empty(title: title, icon: icon)
            : ConversationPane(
                // Fresh state per session (scroll, composer, subscriptions).
                key: ValueKey(selected),
                sessionId: selected,
                onBack: wide
                    ? null
                    : () => ref
                          .read(selectedSessionProvider.notifier)
                          .select(null),
              );
        if (!wide) {
          return selected == null ? const SessionsPane() : conversation;
        }
        return Row(
          children: [
            const SizedBox(width: _sessionsWidth, child: SessionsPane()),
            VerticalDivider(width: 1, color: context.hk.border),
            Expanded(child: conversation),
          ],
        );
      },
    );
  }
}

class _Empty extends ConsumerWidget {
  const _Empty({required this.title, required this.icon});

  final String title;
  final IconData icon;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final err = ref.watch(backendErrorProvider);
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(HkTokens.s6),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 40, color: hk.textMuted),
            const SizedBox(height: HkTokens.s3),
            Text(
              err ?? 'Select a session or start a new one.',
              textAlign: TextAlign.center,
              style: TextStyle(color: err == null ? hk.textMuted : hk.err),
            ),
            if (err == null) ...[
              const SizedBox(height: HkTokens.s3),
              FilledButton.icon(
                onPressed: () =>
                    ref.read(sessionListProvider.notifier).create(),
                icon: const Icon(Icons.add),
                label: const Text('New session'),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
