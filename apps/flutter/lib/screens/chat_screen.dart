import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../app/breakpoints.dart';
import '../ffi/ffi.dart';
import '../theme/tokens.dart';
import 'chat/chat_providers.dart';
import 'chat/conversation_pane.dart';
import 'chat/inspector_pane.dart';
import 'chat/sessions_pane.dart';

/// Chat tab: sessions list + conversation. Two panes on wide windows, list ->
/// conversation navigation on narrow ones (DESIGN §4.3).
class ChatScreen extends ConsumerWidget {
  const ChatScreen({super.key, required this.title, required this.icon});

  final String title;
  final IconData icon;

  static const double _sessionsWidth = 280;
  static const double _inspectorWidth = 320;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final selected = ref.watch(selectedSessionProvider);
    return LayoutBuilder(
      builder: (context, c) {
        final wide = c.maxWidth >= kChatTwoPaneMin;
        final inline = c.maxWidth >= kChatInspectorMin;
        final conversation = selected == null
            ? _Empty(title: title, icon: icon)
            : ConversationPane(
                // Fresh state per session (scroll, composer, subscriptions).
                key: ValueKey(selected),
                sessionId: selected,
                inspectorInline: inline,
                onBack: wide
                    ? null
                    : () => ref
                          .read(selectedSessionProvider.notifier)
                          .select(null),
              );
        if (!wide) {
          if (selected == null) return const SessionsPane();
          // System back returns to the session list instead of leaving the app.
          return PopScope(
            canPop: false,
            onPopInvokedWithResult: (didPop, _) {
              if (!didPop) {
                ref.read(selectedSessionProvider.notifier).select(null);
              }
            },
            child: conversation,
          );
        }
        return Row(
          children: [
            const SizedBox(width: _sessionsWidth, child: SessionsPane()),
            VerticalDivider(width: 1, color: context.hk.border),
            Expanded(child: conversation),
            if (inline &&
                selected != null &&
                ref.watch(inspectorOpenProvider)) ...[
              VerticalDivider(width: 1, color: context.hk.border),
              SizedBox(
                width: _inspectorWidth,
                child: InspectorPane(
                  key: ValueKey(selected),
                  sessionId: selected,
                ),
              ),
            ],
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
