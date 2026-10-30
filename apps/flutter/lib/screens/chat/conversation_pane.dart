import 'dart:convert';
import 'dart:typed_data';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/activity.dart';
import '../../ffi/hk_gate.dart' as gate;

import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_model.dart';
import 'chat_providers.dart';
import 'composer.dart';
import 'inspector_pane.dart';
import 'message_widgets.dart';
import 'sessions_pane.dart' show sessionTitlesProvider;
import 'tool_card.dart';
import '../../widgets/hk_ui.dart';

/// Header + streaming message list + composer for one session.
class ConversationPane extends ConsumerWidget {
  const ConversationPane({
    super.key,
    required this.sessionId,
    this.onBack,
    this.inspectorInline = false,
    this.promptPrefix,
  });

  final String sessionId;

  /// Set on narrow layouts to return to the session list.
  final VoidCallback? onBack;

  /// Inspector is a side column (header button toggles it) rather than a sheet.
  final bool inspectorInline;

  /// Text added before every prompt the user sends from this pane.
  final String Function()? promptPrefix;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final state = ref.watch(chatControllerProvider(sessionId));
    final info = ref
        .watch(sessionListProvider)
        .value
        ?.where((s) => s.id == sessionId)
        .firstOrNull;
    return ColoredBox(
      color: context.hk.bg,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          _Header(
            sessionId: sessionId,
            state: state,
            info: info,
            onBack: onBack,
            inspectorInline: inspectorInline,
          ),
          Expanded(
            child: state.loading
                ? const Center(child: CircularProgressIndicator())
                : _MessageList(
                    sessionId: sessionId,
                    state: state,
                    promptPrefix: promptPrefix,
                  ),
          ),
          Composer(
            sessionId: sessionId,
            running: state.running,
            promptPrefix: promptPrefix,
          ),
        ],
      ),
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
    final custom = ref.watch(sessionTitlesProvider).value?[sessionId];
    final title = custom ?? state.title ?? info?.title;
    final used = state.inputTokens + state.outputTokens;
    return Container(
      height: 52,
      padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(bottom: BorderSide(color: hk.border)),
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
                  (title == null || title.isEmpty) ? 'New chat' : title,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    fontSize: 14,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                Text(
                  [
                    if (info?.model != null) info!.model!,
                    if (used > 0)
                      '${_k(state.inputTokens)} in · ${_k(state.outputTokens)} out',
                  ].join(' · '),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 11, color: hk.textFaint),
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
            Builder(
              builder: (context) {
                final live = ref.watch(sessionLiveProvider(sessionId));
                return live == null
                    ? const SizedBox()
                    : Padding(
                        padding: const EdgeInsets.only(right: HkTokens.s2),
                        child: Text(
                          liveLine(live),
                          style: TextStyle(fontSize: 12, color: hk.textMuted),
                        ),
                      );
              },
            ),
            IconButton(
              tooltip: 'Pause after this step',
              icon: const Icon(Icons.pause_circle_outline),
              onPressed: () =>
                  gate.hk('session.pause', {'session_id': sessionId}),
            ),
            OutlinedButton.icon(
              onPressed: () =>
                  ref.read(chatControllerProvider(sessionId).notifier).cancel(),
              icon: const Icon(Icons.stop, size: 16),
              label: const Text('Stop'),
            ),
            const SizedBox(width: HkTokens.s2),
          ],
          _ContextMeter(sessionId: sessionId, state: state, info: info),
          PopupMenuButton<String>(
            tooltip: 'Export',
            icon: const Icon(Icons.ios_share, size: 19),
            onSelected: (v) async {
              final md = ref
                  .read(chatControllerProvider(sessionId).notifier)
                  .toMarkdown(
                    (title == null || title.isEmpty) ? 'Chat' : title,
                  );
              if (v == 'copy') {
                await Clipboard.setData(ClipboardData(text: md));
                if (context.mounted) {
                  ScaffoldMessenger.of(context).showSnackBar(
                    const SnackBar(content: Text('Copied as Markdown')),
                  );
                }
              } else {
                final path = await FilePicker.saveFile(
                  dialogTitle: 'Save chat',
                  fileName: 'chat.md',
                  bytes: Uint8List.fromList(utf8.encode(md)),
                );
                if (path != null && context.mounted) {
                  ScaffoldMessenger.of(
                    context,
                  ).showSnackBar(const SnackBar(content: Text('Chat saved')));
                }
              }
            },
            itemBuilder: (_) => const [
              PopupMenuItem(value: 'copy', child: Text('Copy as Markdown')),
              PopupMenuItem(value: 'save', child: Text('Save as .md…')),
            ],
          ),
          IconButton(
            tooltip: 'Run settings & inspector',
            icon: Icon(
              Icons.tune,
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

class _MessageList extends ConsumerWidget {
  const _MessageList({
    required this.sessionId,
    required this.state,
    this.promptPrefix,
  });

  final String sessionId;
  final ChatState state;
  final String Function()? promptPrefix;

  static const _starters = [
    (
      Icons.lightbulb_outline,
      'Explain',
      'Explain how this project is structured',
    ),
    (Icons.bug_report_outlined, 'Debug', 'Find and fix the failing tests'),
    (Icons.edit_note, 'Write', 'Draft a README section for setup'),
    (
      Icons.travel_explore,
      'Research',
      'Summarize the latest changes in the repo',
    ),
  ];

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final items = state.items;
    if (items.isEmpty) {
      return Center(
        child: SingleChildScrollView(
          padding: const EdgeInsets.all(HkTokens.s6),
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 640),
            child: Column(
              children: [
                const BrandMark(size: 44),
                const SizedBox(height: HkTokens.s4),
                Text(
                  'What should we work on?',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                const SizedBox(height: HkTokens.s2),
                Text(
                  'Ask anything, attach images, or use / commands and @ files.',
                  style: TextStyle(color: hk.textMuted, fontSize: 13),
                ),
                const SizedBox(height: HkTokens.s6),
                Wrap(
                  spacing: HkTokens.s3,
                  runSpacing: HkTokens.s3,
                  alignment: WrapAlignment.center,
                  children: [
                    for (final (icon, label, prompt) in _starters)
                      SizedBox(
                        width: 300,
                        child: HoverCard(
                          padding: const EdgeInsets.all(HkTokens.s3),
                          onTap: () => ref
                              .read(chatControllerProvider(sessionId).notifier)
                              .send(
                                promptPrefix == null
                                    ? prompt
                                    : '${promptPrefix!()}\n$prompt',
                                ref.read(chatModeProvider),
                                model: ref.read(composerModelProvider),
                              ),
                          child: Row(
                            children: [
                              Icon(icon, size: 18, color: hk.accent),
                              const SizedBox(width: HkTokens.s3),
                              Expanded(
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      label,
                                      style: const TextStyle(
                                        fontSize: 13,
                                        fontWeight: FontWeight.w600,
                                      ),
                                    ),
                                    Text(
                                      prompt,
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: TextStyle(
                                        fontSize: 12,
                                        color: hk.textMuted,
                                      ),
                                    ),
                                  ],
                                ),
                              ),
                            ],
                          ),
                        ),
                      ),
                  ],
                ),
              ],
            ),
          ),
        ),
      );
    }
    final lastAnswer = items.lastIndexWhere((e) => e is AssistantItem);
    final ctl = ref.read(chatControllerProvider(sessionId).notifier);
    // Reversed list: offset 0 is the newest item, so new content stays in
    // view while streaming without any scroll bookkeeping.
    return ListView.separated(
      reverse: true,
      keyboardDismissBehavior: ScrollViewKeyboardDismissBehavior.onDrag,
      padding: const EdgeInsets.symmetric(
        horizontal: HkTokens.s5,
        vertical: HkTokens.s5,
      ),
      itemCount: items.length,
      separatorBuilder: (_, _) => const SizedBox(height: HkTokens.s5),
      itemBuilder: (context, i) {
        final item = items[items.length - 1 - i];
        final w = switch (item) {
          UserItem() => UserBubble(
            item: item,
            onEdit: () => ref
                .read(composerDraftProvider.notifier)
                .put(sessionId, item.text),
          ),
          AssistantItem() => AssistantBlock(
            item: item,
            onRegenerate: items.length - 1 - i == lastAnswer && !state.running
                ? () => ctl.regenerate(
                    ref.read(chatModeProvider),
                    model: ref.read(composerModelProvider),
                  )
                : null,
          ),
          ToolItem() => Padding(
            padding: const EdgeInsets.only(left: 38),
            child: ToolCard(
              key: ValueKey(item.callId),
              sessionId: sessionId,
              item: item,
            ),
          ),
          NoticeItem() => NoticeLine(item: item),
        };
        return Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: kMessageMaxWidth),
            child: w,
          ),
        );
      },
    );
  }
}

/// How full the model's context window was on the last request.
class _ContextMeter extends ConsumerWidget {
  const _ContextMeter({
    required this.sessionId,
    required this.state,
    required this.info,
  });

  final String sessionId;
  final ChatState state;
  final SessionInfoDto? info;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final model = ref.watch(composerModelProvider) ?? info?.model;
    final models = ref.watch(modelListProvider).value?.models ?? const [];
    final window = models
        .where((m) => m.reference == model)
        .firstOrNull
        ?.contextWindow
        .toInt();
    final used = state.lastInputTokens;
    if (window == null || window <= 0 || used <= 0) {
      return const SizedBox.shrink();
    }
    final f = (used / window).clamp(0.0, 1.0);
    final color = f > 0.85 ? hk.err : (f > 0.6 ? hk.warn : hk.accent);
    return Tooltip(
      message:
          'Context: ${_k(used)} of ${_k(window)} tokens (${(f * 100).round()}%)',
      child: Padding(
        padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
        child: SizedBox(
          width: 22,
          height: 22,
          child: CircularProgressIndicator(
            value: f,
            strokeWidth: 3,
            color: color,
            backgroundColor: hk.surface3,
          ),
        ),
      ),
    );
  }
}
