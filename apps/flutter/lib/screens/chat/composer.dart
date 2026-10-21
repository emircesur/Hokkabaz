import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';

/// Minimal composer: text field, send, plan/build toggle. Extras land in P3.3.
class Composer extends ConsumerStatefulWidget {
  const Composer({super.key, required this.sessionId, required this.running});

  final String sessionId;
  final bool running;

  @override
  ConsumerState<Composer> createState() => _ComposerState();
}

class _ComposerState extends ConsumerState<Composer> {
  final _text = TextEditingController();
  final _focus = FocusNode();

  @override
  void dispose() {
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _send() {
    final t = _text.text;
    if (t.trim().isEmpty) return;
    _text.clear();
    ref
        .read(chatControllerProvider(widget.sessionId).notifier)
        .send(t, ref.read(chatModeProvider));
    _focus.requestFocus();
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final mode = ref.watch(chatModeProvider);
    return Container(
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(top: BorderSide(color: hk.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              SegmentedButton<ModeDto>(
                showSelectedIcon: false,
                style: const ButtonStyle(visualDensity: VisualDensity.compact),
                segments: const [
                  ButtonSegment(value: ModeDto.plan, label: Text('Plan')),
                  ButtonSegment(value: ModeDto.build, label: Text('Build')),
                ],
                selected: {mode},
                onSelectionChanged: (s) =>
                    ref.read(chatModeProvider.notifier).set(s.first),
              ),
              if (widget.running) ...[
                const SizedBox(width: HkTokens.s3),
                Text(
                  'Running: your message steers the current turn',
                  style: TextStyle(fontSize: 12, color: hk.textMuted),
                ),
              ],
            ],
          ),
          const SizedBox(height: HkTokens.s2),
          Row(
            crossAxisAlignment: CrossAxisAlignment.end,
            children: [
              Expanded(
                child: CallbackShortcuts(
                  bindings: {
                    // Enter sends; Shift+Enter keeps the newline.
                    const SingleActivator(LogicalKeyboardKey.enter): _send,
                  },
                  child: TextField(
                    controller: _text,
                    focusNode: _focus,
                    minLines: 1,
                    maxLines: 6,
                    textInputAction: TextInputAction.newline,
                    decoration: const InputDecoration(
                      isDense: true,
                      hintText: 'Message',
                      border: OutlineInputBorder(),
                    ),
                  ),
                ),
              ),
              const SizedBox(width: HkTokens.s2),
              IconButton.filled(
                tooltip: 'Send',
                icon: const Icon(Icons.arrow_upward),
                onPressed: _send,
              ),
            ],
          ),
        ],
      ),
    );
  }
}
