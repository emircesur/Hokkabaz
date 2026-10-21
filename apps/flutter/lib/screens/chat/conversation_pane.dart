import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_model.dart';
import 'chat_providers.dart';
import 'composer.dart';
import 'inspector_pane.dart';
import 'message_widgets.dart';
import 'tool_card.dart';

/// Header + streaming message list + composer for one session.
class ConversationPane extends ConsumerWidget {
  const ConversationPane({
    super.key,
    required this.sessionId,
    this.onBack,
    this.inspectorInline = false,
  });

  final String sessionId;

  /// Set on narrow layouts to return to the session list.
  final VoidCallback? onBack;

  /// Inspector is a side column (header button toggles it) rather than a sheet.
  final bool inspectorInline;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(chatControllerProvider(sessionId));
    final info = ref
        .watch(sessionListProvider)
        .value
        ?.where((s) => s.id == sessionId)
        .firstOrNull;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        _Header(
          sessionId: sessionId,
          state: state,
          info: info,
          onBack: onBack,
          inspectorInline: inspectorInline,
        ),
        Divider(height: 1, color: context.hk.border),
        Expanded(
          child: state.loading
              ? const Center(child: CircularProgressIndicator())
              : _MessageList(sessionId: sessionId, state: state),
        ),
        Composer(sessionId: sessionId, running: state.running),
      ],
    );
  }
}

class _Header extends ConsumerWidget {
  const _Header({
    required this.sessionId,
    required this.state,
    required this.info,
    required this.onBack,
    required this.inspectorInline,
  });

  final String sessionId;
  final ChatState state;
  final SessionInfoDto? info;
  final VoidCallback? onBack;
  final bool inspectorInline;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final title = state.title ?? info?.title;
    final used = state.inputTokens + state.outputTokens;
    return Padding(
      padding: const EdgeInsets.symmetric(
        horizontal: HkTokens.s2,
        vertical: HkTokens.s2,
      ),
      child: Row(
        children: [
          if (onBack != null)
            IconButton(
              tooltip: 'Sessions',
              icon: const Icon(Icons.arrow_back),
              onPressed: onBack,
            )
          else
            const SizedBox(width: HkTokens.s2),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  (title == null || title.isEmpty) ? 'New session' : title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.titleMedium,
                ),
                Text(
                  [
                    if (info?.model != null) info!.model!,
                    if (used > 0)
                      '${_k(state.inputTokens)} in · ${_k(state.outputTokens)} out',
                  ].join(' · '),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 12, color: hk.textMuted),
                ),
              ],
            ),
          ),
          if (state.running) ...[
            SizedBox(
              width: 14,
              height: 14,
              child: CircularProgressIndicator(
                strokeWidth: 2,
                color: hk.accent,
              ),
            ),
            const SizedBox(width: HkTokens.s2),
            OutlinedButton.icon(
              onPressed: () =>
                  ref.read(chatControllerProvider(sessionId).notifier).cancel(),
              icon: const Icon(Icons.stop, size: 16),
              label: const Text('Cancel'),
            ),
            const SizedBox(width: HkTokens.s2),
          ],
          IconButton(
            tooltip: 'Inspector',
            icon: Icon(
              Icons.view_sidebar_outlined,
              color: inspectorInline && ref.watch(inspectorOpenProvider)
                  ? hk.accent
                  : null,
            ),
            onPressed: () => inspectorInline
                ? ref.read(inspectorOpenProvider.notifier).toggle()
                : showInspectorSheet(context, sessionId),
          ),
        ],
      ),
    );
  }
}

String _k(int n) => n >= 1000 ? '${(n / 1000).toStringAsFixed(1)}k' : '$n';

class _MessageList extends StatelessWidget {
  const _MessageList({required this.sessionId, required this.state});

  final String sessionId;
  final ChatState state;

  @override
  Widget build(BuildContext context) {
    final items = state.items;
    if (items.isEmpty) {
      return Center(
        child: Text(
          'Send a message to start.',
          style: TextStyle(color: context.hk.textMuted),
        ),
      );
    }
    // Reversed list: offset 0 is the newest item, so new content stays in
    // view while streaming without any scroll bookkeeping.
    return ListView.separated(
      reverse: true,
      padding: const EdgeInsets.all(HkTokens.s4),
      itemCount: items.length,
      separatorBuilder: (_, _) => const SizedBox(height: HkTokens.s3),
      itemBuilder: (context, i) {
        final item = items[items.length - 1 - i];
        return switch (item) {
          UserItem() => UserBubble(item: item),
          AssistantItem() => AssistantBlock(item: item),
          ToolItem() => ToolCard(
            key: ValueKey(item.callId),
            sessionId: sessionId,
            item: item,
          ),
          NoticeItem() => NoticeLine(item: item),
        };
      },
    );
  }
}
