import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/api/chat.dart' as chat;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart';
import '../chat/conversation_pane.dart';
import '../../src/rust/dto.dart' show ModeDto;
import 'studio_providers.dart';

/// One agent conversation per project, kept for the app's lifetime.
final _sessions = <String, String>{};

final studioAgentSessionProvider = FutureProvider.autoDispose
    .family<String, String>((ref, projectId) async {
      final known = _sessions[projectId];
      if (known != null) return known;
      final name =
          ref.read(studioControllerProvider(projectId)).project?.name ??
          'project';
      final s = await chat.createSession(title: 'Studio: $name');
      _sessions[projectId] = s.id;
      ref.keepAlive();
      return s.id;
    });

const _quick = [
  (
    'Make viral shorts',
    'Find the 5 best moments in this video, apply the shorts-karaoke template, and tell me which one is strongest and why.',
  ),
  (
    'Tighten this clip',
    'Tighten the selected clip: cut filler words and long pauses, keep the meaning. Tell me what you removed.',
  ),
  (
    'Captions + vertical',
    'Make the selected clip vertical 9:16 following the speaker, with bold karaoke captions.',
  ),
  (
    'Write post copy',
    'Write titles, descriptions and hashtags for the selected clip for YouTube Shorts, TikTok and Instagram Reels.',
  ),
  ('Render it', 'Render the selected clip and tell me where the file is.'),
];

/// The Studio agent: a chat whose tools edit this project, live in the editor.
class AgentPanel extends ConsumerWidget {
  const AgentPanel({super.key, required this.projectId});

  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final session = ref.watch(studioAgentSessionProvider(projectId));
    final hk = context.hk;
    return session.when(
      loading: () => const Center(child: CircularProgressIndicator()),
      error: (e, _) => Center(
        child: Padding(
          padding: const EdgeInsets.all(HkTokens.s4),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.error_outline, color: hk.err),
              const SizedBox(height: HkTokens.s2),
              Text(describeError(e), textAlign: TextAlign.center),
              const SizedBox(height: HkTokens.s2),
              OutlinedButton(
                onPressed: () =>
                    ref.invalidate(studioAgentSessionProvider(projectId)),
                child: const Text('Retry'),
              ),
            ],
          ),
        ),
      ),
      data: (sid) {
        String prefix() {
          final s = ref.read(studioControllerProvider(projectId));
          return '[Studio: project=$projectId${s.selectedClip == null ? '' : ' clip=${s.selectedClip}'}. Use the studio_* tools; the user watches the editor update live.]';
        }

        return Column(
          children: [
            SizedBox(
              height: 44,
              child: ListView(
                scrollDirection: Axis.horizontal,
                padding: const EdgeInsets.symmetric(
                  horizontal: HkTokens.s2,
                  vertical: 6,
                ),
                children: [
                  for (final q in _quick)
                    Padding(
                      padding: const EdgeInsets.only(right: 6),
                      child: ActionChip(
                        label: Text(q.$1),
                        onPressed: () => ref
                            .read(chatControllerProvider(sid).notifier)
                            .send('${prefix()}\n${q.$2}', ModeDto.build),
                      ),
                    ),
                ],
              ),
            ),
            Divider(height: 1, color: hk.border),
            Expanded(
              child: ConversationPane(sessionId: sid, promptPrefix: prefix),
            ),
          ],
        );
      },
    );
  }
}
