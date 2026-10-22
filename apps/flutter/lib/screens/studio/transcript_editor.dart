import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'player_controller.dart';
import 'studio_models.dart';
import 'studio_providers.dart';
import 'transcript_logic.dart';

/// Text-based editing: click a word to jump there, select words and delete them
/// (the video skips them immediately), restore with another click.
class TranscriptEditor extends ConsumerStatefulWidget {
  const TranscriptEditor({super.key, required this.projectId, required this.player});

  final String projectId;
  final EdlPlayerController player;

  @override
  ConsumerState<TranscriptEditor> createState() => _TranscriptEditorState();
}

class _TranscriptEditorState extends ConsumerState<TranscriptEditor> {
  final _focus = FocusNode(debugLabel: 'transcript');
  final _search = TextEditingController();
  final _active = ValueNotifier<int?>(null);
  WordSelection? _sel;
  bool _extendMode = false;
  List<int> _hits = const [];
  int _hit = 0;
  String? _clipId;

  @override
  void initState() {
    super.initState();
    widget.player.outTime.addListener(_syncActive);
  }

  @override
  void dispose() {
    widget.player.outTime.removeListener(_syncActive);
    _focus.dispose();
    _search.dispose();
    _active.dispose();
    super.dispose();
  }

  List<PreviewWord> get _words => ref.read(studioControllerProvider(widget.projectId)).plan?.words ?? const [];

  void _syncActive() {
    final pos = activeWordPos(_words, widget.player.outTime.value);
    if (pos != _active.value) _active.value = pos;
  }

  StudioController get _ctl => ref.read(studioControllerProvider(widget.projectId).notifier);

  void _tap(int pos, bool extend) {
    final w = _words[pos];
    setState(() {
      _sel = (extend || _extendMode) && _sel != null ? _sel!.extendTo(pos) : WordSelection(pos, pos);
    });
    if (w.outStart != null && !(extend || _extendMode)) widget.player.seekOut(w.outStart!);
    _focus.requestFocus();
  }

  Future<void> _deleteSelection() async {
    final s = ref.read(studioControllerProvider(widget.projectId));
    final clip = s.clip;
    final sel = _sel;
    if (clip == null || sel == null) return;
    final ops = deleteOrRestoreOps(clip.id, _words, sel, clip.deletedWords);
    if (ops.isEmpty) return;
    await _ctl.edit(ops);
    if (mounted) setState(() => _sel = WordSelection(sel.lo, sel.lo));
  }

  void _search_(String q) {
    final hits = searchWords(_words, q);
    setState(() {
      _hits = hits;
      _hit = 0;
    });
    if (hits.isNotEmpty) _jumpTo(hits.first);
  }

  void _jumpTo(int pos) {
    setState(() => _sel = WordSelection(pos, pos));
    final w = _words[pos];
    if (w.outStart != null) widget.player.seekOut(w.outStart!);
  }

  KeyEventResult _key(FocusNode node, KeyEvent e) {
    if (e is! KeyDownEvent) return KeyEventResult.ignored;
    final k = e.logicalKey;
    final ctrl = HardwareKeyboard.instance.isControlPressed || HardwareKeyboard.instance.isMetaPressed;
    final shift = HardwareKeyboard.instance.isShiftPressed;
    if (k == LogicalKeyboardKey.delete || k == LogicalKeyboardKey.backspace) {
      _deleteSelection();
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.space) {
      widget.player.toggle();
      return KeyEventResult.handled;
    }
    if (ctrl && k == LogicalKeyboardKey.keyZ) {
      shift ? _ctl.redo() : _ctl.undo();
      return KeyEventResult.handled;
    }
    if (ctrl && k == LogicalKeyboardKey.keyY) {
      _ctl.redo();
      return KeyEventResult.handled;
    }
    return KeyEventResult.ignored;
  }

  @override
  Widget build(BuildContext context) {
    final state = ref.watch(studioControllerProvider(widget.projectId));
    final plan = state.plan;
    final clip = state.clip;
    final hk = context.hk;
    if (clipChanged(clip?.id)) {
      _sel = null;
      _hits = const [];
    }
    if (clip == null || plan == null) {
      return const Center(child: Text('Select a clip to edit its transcript.'));
    }
    if (!(state.project?.hasTranscript ?? false)) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Text('No transcript yet.'),
            const SizedBox(height: HkTokens.s2),
            FilledButton.icon(
              onPressed: state.busy.contains('transcribe') ? null : _ctl.transcribe,
              icon: state.busy.contains('transcribe')
                  ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                  : const Icon(Icons.subtitles_outlined),
              label: Text(state.busy.contains('transcribe') ? 'Transcribing…' : 'Transcribe'),
            ),
          ],
        ),
      );
    }
    final words = plan.words;
    final paras = paragraphs(words);
    final sel = _sel;
    final canAct = sel != null && sel.hi < words.length;
    final allDeleted = canAct && [for (var i = sel.lo; i <= sel.hi; i++) words[i]].every((w) => w.deleted);
    return Focus(
      focusNode: _focus,
      onKeyEvent: _key,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2, vertical: HkTokens.s1),
            child: Wrap(
              spacing: HkTokens.s2,
              runSpacing: HkTokens.s1,
              crossAxisAlignment: WrapCrossAlignment.center,
              children: [
                SizedBox(
                  width: 180,
                  height: 36,
                  child: TextField(
                    controller: _search,
                    onChanged: _search_,
                    style: Theme.of(context).textTheme.bodyMedium,
                    decoration: InputDecoration(
                      isDense: true,
                      hintText: 'Find in transcript',
                      prefixIcon: const Icon(Icons.search, size: 18),
                      suffixText: _hits.isEmpty ? null : '${_hit + 1}/${_hits.length}',
                      border: const OutlineInputBorder(),
                    ),
                    onSubmitted: (_) {
                      if (_hits.isEmpty) return;
                      setState(() => _hit = (_hit + 1) % _hits.length);
                      _jumpTo(_hits[_hit]);
                    },
                  ),
                ),
                FilledButton.tonalIcon(
                  onPressed: canAct ? _deleteSelection : null,
                  icon: Icon(allDeleted ? Icons.restore : Icons.delete_outline, size: 18),
                  label: Text(allDeleted ? 'Restore' : 'Delete'),
                ),
                OutlinedButton(
                  onPressed: sel == null
                      ? null
                      : () {
                          final (a, b) = sentenceAround(words, sel.lo);
                          setState(() => _sel = WordSelection(a, b));
                        },
                  child: const Text('Sentence'),
                ),
                FilterChip(
                  label: const Text('Select range'),
                  selected: _extendMode,
                  onSelected: (v) => setState(() => _extendMode = v),
                  tooltip: 'Tap to extend the selection (Shift+click on desktop)',
                ),
                FilterChip(
                  label: const Text('Cut fillers'),
                  selected: clip.removeFillers,
                  onSelected: (v) => _ctl.edit([
                    {'op': 'set_fillers', 'clip': clip.id, 'on': v},
                  ]),
                ),
                FilterChip(
                  label: const Text('Cut pauses'),
                  selected: clip.silence != null,
                  onSelected: (v) => _ctl.edit([
                    {
                      'op': 'set_silence',
                      'clip': clip.id,
                      'silence': v ? {'min_gap': 0.6, 'keep': 0.12} : null,
                    },
                  ]),
                ),
              ],
            ),
          ),
          Divider(height: 1, color: hk.border),
          Expanded(
            child: ListView.builder(
              padding: const EdgeInsets.all(HkTokens.s3),
              itemCount: paras.length,
              itemBuilder: (context, i) {
                final (a, b) = paras[i];
                return Padding(
                  padding: const EdgeInsets.only(bottom: HkTokens.s3),
                  child: Wrap(
                    spacing: 2,
                    runSpacing: 4,
                    children: [
                      for (var p = a; p <= b; p++)
                        _WordChip(
                          word: words[p],
                          selected: sel?.contains(p) ?? false,
                          matched: _hits.contains(p),
                          active: _active,
                          pos: p,
                          explicit: clip.deletedWords.contains(words[p].idx),
                          onTap: (extend) => _tap(p, extend),
                        ),
                    ],
                  ),
                );
              },
            ),
          ),
        ],
      ),
    );
  }

  bool clipChanged(String? id) {
    if (id == _clipId) return false;
    _clipId = id;
    return true;
  }
}

class _WordChip extends StatelessWidget {
  const _WordChip({
    required this.word,
    required this.selected,
    required this.matched,
    required this.active,
    required this.pos,
    required this.explicit,
    required this.onTap,
  });

  final PreviewWord word;
  final bool selected;
  final bool matched;
  final ValueNotifier<int?> active;
  final int pos;
  final bool explicit;
  final void Function(bool extend) onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return ValueListenableBuilder<int?>(
      valueListenable: active,
      builder: (context, a, _) {
        final isActive = a == pos;
        final bg = selected
            ? hk.accent2.withValues(alpha: 0.35)
            : isActive
            ? hk.accent.withValues(alpha: 0.45)
            : matched
            ? hk.info.withValues(alpha: 0.25)
            : Colors.transparent;
        final color = word.deleted ? hk.err.withValues(alpha: 0.7) : hk.text;
        return Tooltip(
          message: word.deleted && !explicit ? 'Removed by "Cut fillers"' : '',
          child: GestureDetector(
            onTap: () => onTap(HardwareKeyboard.instance.isShiftPressed || HardwareKeyboard.instance.isControlPressed),
            child: MouseRegion(
              cursor: SystemMouseCursors.click,
              child: Container(
                padding: const EdgeInsets.symmetric(horizontal: 3, vertical: 1),
                decoration: BoxDecoration(color: bg, borderRadius: BorderRadius.circular(4)),
                child: Text(
                  word.text,
                  style: TextStyle(
                    color: color,
                    fontWeight: isActive ? FontWeight.w700 : FontWeight.w400,
                    decoration: word.deleted
                        ? TextDecoration.lineThrough
                        : word.filler
                        ? TextDecoration.underline
                        : null,
                    decorationColor: word.deleted ? hk.err : hk.warn,
                    decorationStyle: word.filler && !word.deleted ? TextDecorationStyle.dashed : null,
                  ),
                ),
              ),
            ),
          ),
        );
      },
    );
  }
}
