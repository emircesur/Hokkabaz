import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'caption_overlay.dart';
import 'studio_providers.dart';

const _swatches = ['#FFFFFF', '#FFE600', '#39FF14', '#00E5FF', '#FF4081', '#FF9100', '#B388FF', '#000000'];

Map<String, dynamic> defaultReframe({bool square = false}) => {
  'aspect': square ? [1, 1] : [9, 16],
  'size': square ? [1080, 1080] : [1080, 1920],
  'mode': 'center',
  'smoothing': 0.5,
  'zoom': 1.0,
  'focus': <Object?>[],
};

/// Look of the selected clip: template, captions, framing, cleanup, B-roll.
class StylePanel extends ConsumerWidget {
  const StylePanel({super.key, required this.projectId});

  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(studioControllerProvider(projectId));
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final clip = s.clip;
    final hk = context.hk;
    if (clip == null) return const Center(child: Text('Select a clip.'));
    final catalog = ref.watch(studioCatalogProvider).value ?? const {};
    final templates = ((catalog['templates'] as List?) ?? const []).cast<Map>();
    final presets = ((catalog['caption_presets'] as List?) ?? const ['classic', 'karaoke', 'pop', 'minimal']).cast<String>();

    Future<void> setCaptions(Map<String, dynamic>? style) => ctl.edit([
      {'op': 'set_captions', 'clip': clip.id, 'style': style},
    ]);
    Future<void> setReframe(Map<String, dynamic>? r) => ctl.edit([
      {'op': 'set_reframe', 'clip': clip.id, 'reframe': r},
    ]);
    final cap = clip.captions;
    Map<String, dynamic> capWith(String k, Object? v) => {...?cap, k: v};

    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        _Section(
          'Template',
          child: DropdownButtonFormField<String>(
            initialValue: templates.any((t) => t['name'] == clip.template) ? clip.template : null,
            isExpanded: true,
            decoration: const InputDecoration(isDense: true, hintText: 'Apply a template', border: OutlineInputBorder()),
            items: [
              for (final t in templates)
                DropdownMenuItem(
                  value: t['name'] as String,
                  child: Tooltip(message: t['description'] as String? ?? '', child: Text(t['name'] as String)),
                ),
            ],
            onChanged: (v) {
              if (v != null) {
                ctl.edit([
                  {'op': 'apply_template', 'clip': clip.id, 'template': v},
                ]);
              }
            },
          ),
        ),
        _Section(
          'Captions',
          trailing: Switch(value: cap != null, onChanged: (v) => v ? ctl.edit([{'op': 'set_caption_preset', 'clip': clip.id, 'preset': 'karaoke'}]) : setCaptions(null)),
          child: cap == null
              ? Text('Off', style: TextStyle(color: hk.textMuted))
              : Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Wrap(
                      spacing: 6,
                      children: [
                        for (final p in presets)
                          ActionChip(
                            label: Text(p),
                            onPressed: () => ctl.edit([
                              {'op': 'set_caption_preset', 'clip': clip.id, 'preset': p},
                            ]),
                          ),
                      ],
                    ),
                    const SizedBox(height: HkTokens.s2),
                    SegmentedButton<String>(
                      showSelectedIcon: false,
                      segments: const [
                        ButtonSegment(value: 'block', label: Text('Line')),
                        ButtonSegment(value: 'karaoke', label: Text('Karaoke')),
                        ButtonSegment(value: 'word_by_word', label: Text('Word')),
                      ],
                      selected: {(cap['mode'] as String?) ?? 'block'},
                      onSelectionChanged: (v) => setCaptions(capWith('mode', v.first)),
                    ),
                    const SizedBox(height: HkTokens.s2),
                    SegmentedButton<String>(
                      showSelectedIcon: false,
                      segments: const [
                        ButtonSegment(value: 'top', label: Text('Top')),
                        ButtonSegment(value: 'center', label: Text('Middle')),
                        ButtonSegment(value: 'bottom', label: Text('Bottom')),
                      ],
                      selected: {(cap['pos'] as String?) ?? 'bottom'},
                      onSelectionChanged: (v) => setCaptions(capWith('pos', v.first)),
                    ),
                    _Labeled('Size', Slider(min: 24, max: 160, value: ((cap['size'] as num?) ?? 56).toDouble().clamp(24, 160), onChangeEnd: (v) => setCaptions(capWith('size', v.round())), onChanged: (_) {})),
                    _Labeled('Words per line', Slider(min: 1, max: 12, divisions: 11, value: ((cap['max_words'] as num?) ?? 7).toDouble().clamp(1, 12), onChangeEnd: (v) => setCaptions(capWith('max_words', v.round())), onChanged: (_) {})),
                    const Text('Text colour'),
                    _Swatches(selected: cap['primary'] as String?, onPick: (c) => setCaptions(capWith('primary', c))),
                    const SizedBox(height: 4),
                    const Text('Highlight colour'),
                    _Swatches(selected: cap['highlight'] as String?, onPick: (c) => setCaptions(capWith('highlight', c))),
                    Row(
                      children: [
                        const Expanded(child: Text('UPPERCASE')),
                        Switch(value: cap['uppercase'] == true, onChanged: (v) => setCaptions(capWith('uppercase', v))),
                      ],
                    ),
                  ],
                ),
        ),
        _Section(
          'Framing',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SegmentedButton<String>(
                showSelectedIcon: false,
                segments: const [
                  ButtonSegment(value: 'off', label: Text('16:9')),
                  ButtonSegment(value: 'vertical', label: Text('9:16')),
                  ButtonSegment(value: 'square', label: Text('1:1')),
                ],
                selected: {clip.aspectName == 'vertical' || clip.aspectName == 'square' ? clip.aspectName! : 'off'},
                onSelectionChanged: (v) => switch (v.first) {
                  'off' => setReframe(null),
                  'square' => setReframe({...defaultReframe(square: true), if (clip.reframe != null) 'focus': clip.reframe!['focus']}),
                  _ => setReframe({...defaultReframe(), if (clip.reframe != null) 'focus': clip.reframe!['focus']}),
                },
              ),
              if (clip.reframe != null) ...[
                const SizedBox(height: HkTokens.s2),
                FilledButton.tonalIcon(
                  onPressed: s.busy.contains('reframe') ? null : () => ctl.reframe(clip.id, square: clip.aspectName == 'square'),
                  icon: s.busy.contains('reframe')
                      ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Icon(Icons.center_focus_strong_outlined, size: 18),
                  label: Text(clip.tracked ? 'Re-track speaker' : 'Follow the speaker'),
                ),
                _Labeled('Camera smoothness', Slider(min: 0, max: 0.95, value: ((clip.reframe!['smoothing'] as num?) ?? 0.5).toDouble().clamp(0, 0.95), onChangeEnd: (v) => setReframe({...clip.reframe!, 'smoothing': v}), onChanged: (_) {})),
                _Labeled('Zoom', Slider(min: 1, max: 2.5, value: ((clip.reframe!['zoom'] as num?) ?? 1).toDouble().clamp(1, 2.5), onChangeEnd: (v) => setReframe({...clip.reframe!, 'zoom': v}), onChanged: (_) {})),
              ],
            ],
          ),
        ),
        _Section(
          'Music',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  OutlinedButton.icon(
                    onPressed: () async {
                      final files = await FilePicker.pickFiles(type: FileType.audio, dialogTitle: 'Background music');
                      final path = files.firstOrNull?.path;
                      if (path == null) return;
                      ctl.edit([
                        {
                          'op': 'set_music',
                          'clip': clip.id,
                          'music': {'path': path, 'gain': 0.25, 'duck': true},
                        },
                      ]);
                    },
                    icon: const Icon(Icons.music_note, size: 18),
                    label: Text(clip.music == null ? 'Add music' : 'Change'),
                  ),
                  const Spacer(),
                  if (clip.music != null)
                    TextButton(
                      onPressed: () => ctl.edit([
                        {'op': 'set_music', 'clip': clip.id, 'music': null},
                      ]),
                      child: const Text('Remove'),
                    ),
                ],
              ),
              if (clip.music != null) ...[
                Text((clip.music!['path'] as String? ?? '').split(RegExp(r'[\\/]')).last, maxLines: 1, overflow: TextOverflow.ellipsis, style: Theme.of(context).textTheme.bodySmall),
                _Labeled('Volume', Slider(min: 0.05, max: 1, value: ((clip.music!['gain'] as num?) ?? 0.25).toDouble().clamp(0.05, 1), onChangeEnd: (v) => ctl.edit([{'op': 'set_music', 'clip': clip.id, 'music': {...clip.music!, 'gain': v}}]), onChanged: (_) {})),
                Row(
                  children: [
                    const Expanded(child: Text('Lower under speech')),
                    Switch(value: clip.music!['duck'] != false, onChanged: (v) => ctl.edit([{'op': 'set_music', 'clip': clip.id, 'music': {...clip.music!, 'duck': v}}])),
                  ],
                ),
              ],
            ],
          ),
        ),
        _Section(
          'B-roll',
          child: Row(
            children: [
              FilledButton.tonalIcon(
                onPressed: s.busy.contains('broll') ? null : () => ctl.broll(clip.id),
                icon: s.busy.contains('broll')
                    ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Icon(Icons.movie_filter_outlined, size: 18),
                label: const Text('Add B-roll'),
              ),
              const SizedBox(width: HkTokens.s2),
              Text('${clip.brollCount} inserted', style: TextStyle(color: hk.textMuted)),
              const Spacer(),
              if (clip.brollCount > 0)
                TextButton(
                  onPressed: () => ctl.edit([
                    {'op': 'set_broll', 'clip': clip.id, 'broll': <Object?>[]},
                  ]),
                  child: const Text('Clear'),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

class _Section extends StatelessWidget {
  const _Section(this.title, {required this.child, this.trailing});

  final String title;
  final Widget child;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      margin: const EdgeInsets.only(bottom: HkTokens.s3),
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(color: hk.surface, border: Border.all(color: hk.border), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(children: [Expanded(child: Text(title, style: Theme.of(context).textTheme.titleSmall)), ?trailing]),
          const SizedBox(height: HkTokens.s2),
          child,
        ],
      ),
    );
  }
}

class _Labeled extends StatelessWidget {
  const _Labeled(this.label, this.child);
  final String label;
  final Widget child;

  @override
  Widget build(BuildContext context) => Row(
    children: [
      SizedBox(width: 110, child: Text(label, style: Theme.of(context).textTheme.bodySmall)),
      Expanded(child: child),
    ],
  );
}

class _Swatches extends StatelessWidget {
  const _Swatches({required this.selected, required this.onPick});

  final String? selected;
  final void Function(String) onPick;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Wrap(
      spacing: 6,
      runSpacing: 6,
      children: [
        for (final c in _swatches)
          InkWell(
            onTap: () => onPick(c),
            customBorder: const CircleBorder(),
            child: Container(
              width: 30,
              height: 30,
              decoration: BoxDecoration(
                color: hexColor(c, Colors.white),
                shape: BoxShape.circle,
                border: Border.all(color: selected?.toUpperCase() == c ? hk.accent : hk.border, width: selected?.toUpperCase() == c ? 3 : 1),
              ),
            ),
          ),
      ],
    );
  }
}
