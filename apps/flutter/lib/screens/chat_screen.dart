import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../app/breakpoints.dart';
import '../ffi/ffi.dart';
import '../theme/tokens.dart';
import 'chat/chat_providers.dart';
import 'chat/conversation_pane.dart';
import 'chat/inspector_pane.dart';
import 'chat/run_settings_pane.dart';
import 'chat/sessions_pane.dart';
import '../widgets/hk_ui.dart';

/// Chat tab: sessions list + conversation. Two panes on wide windows, list ->
/// conversation navigation on narrow ones (DESIGN §4.3).
class ChatScreen extends ConsumerWidget {
  const ChatScreen({super.key, required this.title, required this.icon});

  final String title;
  final IconData icon;

  static const double _sessionsWidth = 272;
  static const double _inspectorWidth = 340;

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
                child: _SidePanel(key: ValueKey(selected), sessionId: selected),
              ),
            ],
          ],
        );
      },
    );
  }
}

/// Right column: per-chat run settings and the inspector, as tabs.
class _SidePanel extends StatelessWidget {
  const _SidePanel({super.key, required this.sessionId});

  final String sessionId;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return ColoredBox(
      color: hk.surface,
      child: DefaultTabController(
        length: 2,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            SizedBox(
              height: 52,
              child: Container(
                decoration: BoxDecoration(
                  border: Border(bottom: BorderSide(color: hk.border)),
                ),
                padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
                alignment: Alignment.bottomLeft,
                child: const TabBar(
                  dividerHeight: 0,
                  tabs: [
                    Tab(height: 40, text: 'Run settings'),
                    Tab(height: 40, text: 'Inspector'),
                  ],
                ),
              ),
            ),
            Expanded(
              child: TabBarView(
                children: [
                  RunSettingsPane(sessionId: sessionId),
                  InspectorPane(sessionId: sessionId),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _Empty extends ConsumerWidget {
  const _Empty({required this.title, required this.icon});

  final String title;
  final IconData icon;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final err = ref.watch(backendErrorProvider);
    return ColoredBox(
      color: context.hk.bg,
      child: EmptyState(
        icon: icon,
        title: err == null ? 'Start a conversation' : 'Backend unavailable',
        message: err ?? 'Pick a chat on the left or start a new one. Any provider or on-device model works.',
        action: err == null
            ? GradientButton(
                label: 'New chat',
                icon: Icons.add,
                onPressed: () =>
                    ref.read(sessionListProvider.notifier).create(),
              )
            : null,
      ),
    );
  }
}
