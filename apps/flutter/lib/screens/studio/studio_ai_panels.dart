import 'dart:convert';
import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/hk_gate.dart';
import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../ai/ai_common.dart';
import 'studio_providers.dart';

/// Render/export preferences shared by the Export panel and AI enhance toggles.
class RenderPrefs extends Notifier<Map<String, dynamic>> {
  @override
  Map<String, dynamic> build() => {
    'encoder': 'software',
    'audio_cleanup': 'none',
    'crf': 20,
  };
  void set(String k, Object? v) => state = {...state, k: v};
}

final renderPrefsProvider = NotifierProvider<RenderPrefs, Map<String, dynamic>>(
  RenderPrefs.new,
);

/// Tool to open when a project is opened from a home-screen shortcut.
class PendingTool extends Notifier<String?> {
  @override
  String? build() => null;
  void set(String? t) => state = t;
}

final studioPendingToolProvider = NotifierProvider<PendingTool, String?>(
  PendingTool.new,
);

Future<dynamic> studioCall(String cmd, Map<String, dynamic> params) =>
    hk('studio.$cmd', params);

class _Head extends StatelessWidget {
  const _Head(this.title, {this.subtitle});
  final String title;
  final String? subtitle;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: HkTokens.s2, top: HkTokens.s2),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(
          title,
          style: const TextStyle(fontWeight: FontWeight.w600, fontSize: 15),
        ),
        if (subtitle != null)
          Text(
            subtitle!,
            style: TextStyle(fontSize: 12, color: context.hk.textMuted),
          ),
      ],
    ),
  );
}

class _ActionRow extends StatelessWidget {
  const _ActionRow({
    required this.icon,
    required this.label,
    this.onTap,
    this.trailing,
    this.busy = false,
  });
  final IconData icon;
  final String label;
  final VoidCallback? onTap;
  final Widget? trailing;
  final bool busy;
  @override
  Widget build(BuildContext context) => Card(
    margin: const EdgeInsets.only(bottom: 6),
    child: ListTile(
      dense: true,
      leading: Icon(icon),
      title: Text(label),
      onTap: busy ? null : onTap,
      trailing: busy
          ? const SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            )
          : trailing,
    ),
  );
}

/// "AI enhance": speech cleanup, punch-in, speech enhancement, speakers, translate, dub.
class AiEnhancePanel extends ConsumerStatefulWidget {
  const AiEnhancePanel({super.key, required this.projectId});
  final String projectId;
  @override
  ConsumerState<AiEnhancePanel> createState() => _AiEnhancePanelState();
}

class _AiEnhancePanelState extends ConsumerState<AiEnhancePanel> {
  final Set<String> _busy = {};
  String? _out;
  bool _punch = false;
  final _lang = TextEditingController(text: 'Turkish');

  Future<void> _run(
    String key,
    Future<dynamic> Function() fn,
    String Function(dynamic) show,
  ) async {
    setState(() => _busy.add(key));
    final r = await guarded(context, fn);
    if (mounted) {
      setState(() {
        _busy.remove(key);
        if (r != null) _out = show(r);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = ref.watch(studioControllerProvider(widget.projectId));
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final prefs = ref.watch(renderPrefsProvider);
    final clip = s.clip;
    final id = widget.projectId;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const _Head('AI enhance'),
        if (clip == null) const Text('Select a clip to edit.'),
        if (clip != null) ...[
          _ActionRow(
            icon: Icons.content_cut,
            label: 'Remove filler words',
            trailing: Switch(
              value: clip.removeFillers,
              onChanged: (v) => ctl.edit([
                {'op': 'set_fillers', 'clip': clip.id, 'on': v},
              ]),
            ),
          ),
          _ActionRow(
            icon: Icons.av_timer,
            label: 'Remove pauses',
            trailing: Switch(
              value: clip.silence != null,
              onChanged: (v) => ctl.edit([
                {
                  'op': 'set_silence',
                  'clip': clip.id,
                  'silence': v ? {'min_gap': 0.6, 'keep': 0.12} : null,
                },
              ]),
            ),
          ),
          _ActionRow(
            icon: Icons.zoom_in_map,
            label: 'Auto zoom on emphasis',
            trailing: Switch(
              value: _punch,
              onChanged: (v) async {
                setState(() => _punch = v);
                await guarded(
                  context,
                  () => studioCall('set_punch_in', {
                    'id': id,
                    'clip': clip.id,
                    'punch_in': v
                        ? {'zoom': 1.15, 'hold': 0.8, 'max_per_minute': 6}
                        : null,
                  }),
                );
                await ctl.reload();
              },
            ),
          ),
        ],
        _ActionRow(
          icon: Icons.graphic_eq,
          label: 'Speech enhancement (on export)',
          trailing: Switch(
            value: prefs['audio_cleanup'] != 'none',
            onChanged: (v) => ref
                .read(renderPrefsProvider.notifier)
                .set('audio_cleanup', v ? 'podcast' : 'none'),
          ),
        ),
        _ActionRow(
          icon: Icons.record_voice_over_outlined,
          label: 'Label speakers',
          busy: _busy.contains('diarize'),
          onTap: () =>
              _run('diarize', () => studioCall('diarize', {'id': id}), (r) {
                final rows = (r as List).cast<Map>();
                return rows
                    .take(60)
                    .map((l) => '${l['speaker']}: ${l['text']}')
                    .join('\n');
              }),
        ),
        _ActionRow(
          icon: Icons.switch_video_outlined,
          label: 'Multicam: auto-switch to active speaker',
          busy: _busy.contains('multicam'),
          onTap: () async {
            final files = await FilePicker.pickFiles(
              dialogTitle: 'Other camera angles (this video is angle 1)',
              type: FileType.video,
            );
            final angles = [
              for (final f in files)
                if (f.path != null) f.path!,
            ];
            if (angles.isEmpty) return;
            await _run(
              'multicam',
              () => studioCall('multicam', {
                'id': id,
                'angles': angles,
                'min_shot_s': 3.0,
              }),
              (r) {
                final shots = (r['shots'] as List).length;
                return 'Multicam edit: $shots shots across ${angles.length + 1} angles\n${r['path']}\n'
                    'Import it from Studio home to clip it.';
              },
            );
          },
        ),
        const _Head('Translate & dub'),
        TextField(
          controller: _lang,
          decoration: const InputDecoration(
            labelText: 'Target language',
            isDense: true,
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 6),
        _ActionRow(
          icon: Icons.translate,
          label: 'Translated captions (SRT)',
          busy: _busy.contains('translate'),
          onTap: () => _run(
            'translate',
            () => studioCall('translate_captions', {
              'id': id,
              'language': _lang.text,
            }),
            (r) => 'Saved: $r',
          ),
        ),
        _ActionRow(
          icon: Icons.spatial_audio_off_outlined,
          label: 'Voice dubbing track',
          busy: _busy.contains('dub'),
          onTap: () => _run('dub', () async {
            final g = await hkMap('genai.get');
            if (g['tts'] == null) {
              throw Exception(
                'Set up text-to-speech in AI hub → AI settings → Generation',
              );
            }
            return studioCall('dub', {
              'id': id,
              'language': _lang.text,
              'tts': g['tts'],
            });
          }, (r) => 'Dub track: $r'),
        ),
        if (_out != null) ...[
          const SizedBox(height: HkTokens.s2),
          _ResultBox(text: _out!),
        ],
      ],
    );
  }
}

class _ResultBox extends StatelessWidget {
  const _ResultBox({required this.text});
  final String text;
  @override
  Widget build(BuildContext context) => Container(
    padding: const EdgeInsets.all(HkTokens.s2),
    decoration: BoxDecoration(
      border: Border.all(color: context.hk.border),
      borderRadius: BorderRadius.circular(HkTokens.radiusCard),
    ),
    child: Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Align(
          alignment: Alignment.centerRight,
          child: IconButton(
            tooltip: 'Copy',
            icon: const Icon(Icons.copy, size: 18),
            onPressed: () => Clipboard.setData(ClipboardData(text: text)),
          ),
        ),
        SelectableText(text),
      ],
    ),
  );
}

/// Long-form writeups: chapters, blog, newsletter, thread, summary, hook review.
class WriteupPanel extends ConsumerStatefulWidget {
  const WriteupPanel({super.key, required this.projectId});
  final String projectId;
  @override
  ConsumerState<WriteupPanel> createState() => _WriteupPanelState();
}

class _WriteupPanelState extends ConsumerState<WriteupPanel> {
  String? _busy;
  String? _out;

  Future<void> _go(
    String key,
    Future<dynamic> Function() fn,
    String Function(dynamic) show,
  ) async {
    setState(() => _busy = key);
    final r = await guarded(context, fn);
    if (mounted) {
      setState(() {
        _busy = null;
        if (r != null) _out = show(r);
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final clip = ref.watch(studioControllerProvider(widget.projectId)).clip;
    final id = widget.projectId;
    String chapters(dynamic r) => (r as List)
        .map((c) {
          final t = ((c as Map)['start'] as num).toInt();
          return '${t ~/ 60}:${(t % 60).toString().padLeft(2, '0')} ${c['title']}';
        })
        .join('\n');
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const _Head(
          'Write from the video',
          subtitle: 'Chapters, articles and posts from the transcript.',
        ),
        _ActionRow(
          icon: Icons.format_list_numbered,
          label: 'YouTube chapters',
          busy: _busy == 'ch',
          onTap: () =>
              _go('ch', () => studioCall('chapters', {'id': id}), chapters),
        ),
        for (final (k, label, icon) in const [
          ('blog', 'Blog post', Icons.article_outlined),
          ('newsletter', 'Newsletter', Icons.mail_outline),
          ('thread', 'Social thread', Icons.forum_outlined),
          ('summary', 'Summary', Icons.short_text),
        ])
          _ActionRow(
            icon: icon,
            label: label,
            busy: _busy == k,
            onTap: () => _go(
              k,
              () => studioCall('longform', {'id': id, 'kind': k}),
              (r) => r.toString(),
            ),
          ),
        const _Head(
          'Voiceover hook',
          subtitle: 'Score the first 3 seconds and get stronger openings.',
        ),
        _ActionRow(
          icon: Icons.bolt,
          label: clip == null
              ? 'Select a clip first'
              : 'Review hook of "${clip.title}"',
          busy: _busy == 'hook',
          onTap: clip == null
              ? null
              : () => _go(
                  'hook',
                  () => studioCall('hook', {'id': id, 'clip': clip.id}),
                  (r) {
                    final m = (r as Map).cast<String, dynamic>();
                    return 'Score ${m['score']}/100\n\nIssues:\n- ${(m['issues'] as List).join('\n- ')}\n\nRewrites:\n- ${(m['rewrites'] as List).join('\n- ')}\n\nTitle card: ${m['title_card'] ?? ''}';
                  },
                ),
        ),
        if (_out != null) ...[
          const SizedBox(height: HkTokens.s2),
          _ResultBox(text: _out!),
        ],
      ],
    );
  }
}

/// Brand kit: logo, colors, font (applied to every render of the project).
class BrandPanel extends ConsumerStatefulWidget {
  const BrandPanel({super.key, required this.projectId});
  final String projectId;
  @override
  ConsumerState<BrandPanel> createState() => _BrandPanelState();
}

class _BrandPanelState extends ConsumerState<BrandPanel> {
  Map<String, dynamic> _b = {
    'name': 'My brand',
    'logo_corner': 'top_right',
    'logo_width': 0.14,
    'logo_opacity': 0.9,
  };

  @override
  void initState() {
    super.initState();
    api
        .studioProject(id: widget.projectId)
        .then((raw) {
          final j = jsonDecode(raw);
          if (mounted && j is Map && j['brand'] is Map) {
            setState(() => _b = (j['brand'] as Map).cast<String, dynamic>());
          }
        })
        .catchError((_) {});
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(HkTokens.s3),
    children: [
      const _Head(
        'Brand template',
        subtitle: 'Logo watermark, caption font and colors for every clip.',
      ),
      Row(
        children: [
          Expanded(
            child: Text(
              _b['logo'] as String? ?? 'No logo',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
          ),
          TextButton.icon(
            icon: const Icon(Icons.image_outlined),
            label: const Text('Logo'),
            onPressed: () async {
              final r = await FilePicker.pickFiles(type: FileType.image);
              final p = r.firstOrNull?.path;
              if (p != null) setState(() => _b['logo'] = p);
            },
          ),
        ],
      ),
      DropdownButtonFormField<String>(
        initialValue: _b['logo_corner'] as String? ?? 'top_right',
        decoration: const InputDecoration(
          labelText: 'Logo corner',
          isDense: true,
          border: OutlineInputBorder(),
        ),
        items: const [
          DropdownMenuItem(value: 'top_left', child: Text('Top left')),
          DropdownMenuItem(value: 'top_right', child: Text('Top right')),
          DropdownMenuItem(value: 'bottom_left', child: Text('Bottom left')),
          DropdownMenuItem(value: 'bottom_right', child: Text('Bottom right')),
        ],
        onChanged: (v) => setState(() => _b['logo_corner'] = v),
      ),
      const SizedBox(height: 8),
      Text(
        'Logo size ${(((_b['logo_width'] as num?) ?? 0.14) * 100).round()}%',
      ),
      Slider(
        value: ((_b['logo_width'] as num?) ?? 0.14).toDouble().clamp(0.04, 0.4),
        min: 0.04,
        max: 0.4,
        onChanged: (v) => setState(() => _b['logo_width'] = v),
      ),
      Text(
        'Logo opacity ${(((_b['logo_opacity'] as num?) ?? 0.9) * 100).round()}%',
      ),
      Slider(
        value: ((_b['logo_opacity'] as num?) ?? 0.9).toDouble().clamp(0.1, 1.0),
        min: 0.1,
        max: 1.0,
        onChanged: (v) => setState(() => _b['logo_opacity'] = v),
      ),
      for (final (k, label) in const [
        ('font', 'Caption font'),
        ('primary_color', 'Text color (#RRGGBB)'),
        ('accent_color', 'Highlight color (#RRGGBB)'),
      ])
        Padding(
          padding: const EdgeInsets.only(bottom: 8),
          child: TextFormField(
            initialValue: _b[k] as String? ?? '',
            decoration: InputDecoration(
              labelText: label,
              isDense: true,
              border: const OutlineInputBorder(),
            ),
            onChanged: (v) => _b[k] = v.trim().isEmpty ? null : v.trim(),
          ),
        ),
      Row(
        children: [
          FilledButton(
            onPressed: () async {
              await guarded(
                context,
                () => studioCall('set_brand', {
                  'id': widget.projectId,
                  'brand': _b,
                }),
                ok: 'Brand applied',
              );
              await ref
                  .read(studioControllerProvider(widget.projectId).notifier)
                  .reload();
            },
            child: const Text('Apply'),
          ),
          const SizedBox(width: 8),
          TextButton(
            onPressed: () => guarded(
              context,
              () => studioCall('set_brand', {
                'id': widget.projectId,
                'brand': null,
              }),
              ok: 'Brand removed',
            ),
            child: const Text('Remove'),
          ),
        ],
      ),
    ],
  );
}

/// Thumbnail A/B variants.
class ThumbnailsPanel extends ConsumerStatefulWidget {
  const ThumbnailsPanel({super.key, required this.projectId});
  final String projectId;
  @override
  ConsumerState<ThumbnailsPanel> createState() => _ThumbnailsPanelState();
}

class _ThumbnailsPanelState extends ConsumerState<ThumbnailsPanel> {
  final _texts = TextEditingController();
  List<String> _paths = const [];
  bool _busy = false;

  @override
  Widget build(BuildContext context) {
    final clip = ref.watch(studioControllerProvider(widget.projectId)).clip;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const _Head(
          'Thumbnails',
          subtitle: 'Best frame per window + headline. One headline per line = one variant.',
        ),
        TextField(
          controller: _texts,
          minLines: 2,
          maxLines: 5,
          decoration: const InputDecoration(
            hintText: 'Headlines (optional)',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: 8),
        FilledButton.icon(
          onPressed: clip == null || _busy
              ? null
              : () async {
                  setState(() => _busy = true);
                  final texts = [
                    for (final l in _texts.text.split('\n'))
                      if (l.trim().isNotEmpty) l.trim(),
                  ];
                  final r = await guarded(
                    context,
                    () => studioCall('thumbnails', {
                      'id': widget.projectId,
                      'clip': clip.id,
                      'texts': texts,
                      'variants': texts.isEmpty ? 3 : texts.length,
                    }),
                  );
                  if (mounted) {
                    setState(() {
                      _busy = false;
                      if (r is List) _paths = r.cast<String>();
                    });
                  }
                },
          icon: const Icon(Icons.photo_library_outlined),
          label: Text(
            clip == null
                ? 'Select a clip'
                : (_busy ? 'Generating…' : 'Generate'),
          ),
        ),
        const SizedBox(height: 8),
        for (final p in _paths)
          Padding(
            padding: const EdgeInsets.only(bottom: 8),
            child: File(p).existsSync()
                ? Image.file(File(p))
                : SelectableText(p),
          ),
      ],
    );
  }
}

/// Export: encoder, speech enhancement, quality; render one clip or a whole folder.
class ExportPanel extends ConsumerWidget {
  const ExportPanel({super.key, required this.projectId});
  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final prefs = ref.watch(renderPrefsProvider);
    final n = ref.read(renderPrefsProvider.notifier);
    final s = ref.watch(studioControllerProvider(projectId));
    final clip = s.clip;
    final opts = {
      'encoder': prefs['encoder'],
      'audio_cleanup': prefs['audio_cleanup'],
      'crf': prefs['crf'],
    };
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const _Head('Export'),
        DropdownButtonFormField<String>(
          initialValue: prefs['encoder'] as String,
          decoration: const InputDecoration(
            labelText: 'Video encoder',
            isDense: true,
            border: OutlineInputBorder(),
          ),
          items: const [
            DropdownMenuItem(value: 'auto', child: Text('Auto (best GPU)')),
            DropdownMenuItem(value: 'software', child: Text('Software (CPU)')),
            DropdownMenuItem(value: 'nvenc', child: Text('NVIDIA NVENC')),
            DropdownMenuItem(value: 'qsv', child: Text('Intel Quick Sync')),
            DropdownMenuItem(value: 'vaapi', child: Text('VAAPI (Linux)')),
            DropdownMenuItem(
              value: 'videotoolbox',
              child: Text('VideoToolbox (macOS)'),
            ),
          ],
          onChanged: (v) => n.set('encoder', v),
        ),
        const SizedBox(height: 8),
        DropdownButtonFormField<String>(
          initialValue: prefs['audio_cleanup'] as String,
          decoration: const InputDecoration(
            labelText: 'Speech enhancement',
            isDense: true,
            border: OutlineInputBorder(),
          ),
          items: const [
            DropdownMenuItem(value: 'none', child: Text('Off')),
            DropdownMenuItem(
              value: 'podcast',
              child: Text('Podcast (denoise, compress, -16 LUFS)'),
            ),
            DropdownMenuItem(
              value: 'social',
              child: Text('Social (light denoise, -14 LUFS)'),
            ),
          ],
          onChanged: (v) => n.set('audio_cleanup', v),
        ),
        const SizedBox(height: 8),
        Text('Quality (CRF ${prefs['crf']}, lower = better)'),
        Slider(
          value: ((prefs['crf'] as num?) ?? 20).toDouble(),
          min: 14,
          max: 32,
          divisions: 18,
          onChanged: (v) => n.set('crf', v.round()),
        ),
        FilledButton.icon(
          onPressed: clip == null
              ? null
              : () => guarded(
                  context,
                  () => studioCall('render', {
                    'id': projectId,
                    'clip': clip.id,
                    'opts': opts,
                  }),
                  ok: 'Rendering — see Activity',
                ),
          icon: const Icon(Icons.file_download_outlined),
          label: Text(
            clip == null ? 'Select a clip' : 'Export "${clip.title}"',
          ),
        ),
        if (clip != null && s.renderProgress[clip.id] != null) ...[
          const SizedBox(height: 8),
          LinearProgressIndicator(value: s.renderProgress[clip.id]),
        ],
        const _Head(
          'Batch',
          subtitle: 'Every video in a folder → transcribe → highlights → render the top clips.',
        ),
        OutlinedButton.icon(
          icon: const Icon(Icons.drive_folder_upload_outlined),
          label: const Text('Process a folder'),
          onPressed: () async {
            final d = await FilePicker.getDirectoryPath(
              dialogTitle: 'Folder with videos',
            );
            if (d == null || !context.mounted) return;
            await guarded(
              context,
              () => studioCall('batch', {'folder': d, 'top': 3, 'opts': opts}),
              ok: 'Batch started — see Activity',
            );
          },
        ),
      ],
    );
  }
}

/// Analytics loop: pull YouTube stats for published clips, suggest rubric weights.
class AnalyticsPanel extends ConsumerStatefulWidget {
  const AnalyticsPanel({super.key, required this.projectId});
  final String projectId;
  @override
  ConsumerState<AnalyticsPanel> createState() => _AnalyticsPanelState();
}

class _AnalyticsPanelState extends ConsumerState<AnalyticsPanel> {
  final Map<String, TextEditingController> _ids = {};
  Map<String, dynamic>? _out;
  bool _busy = false;

  @override
  void dispose() {
    for (final c in _ids.values) {
      c.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final clips =
        ref.watch(studioControllerProvider(widget.projectId)).project?.clips ??
        const [];
    final weights =
        (_out?['suggested_weights'] as Map?)?.cast<String, dynamic>() ??
        const {};
    final videos = [
      for (final v in (_out?['videos'] as List? ?? const []))
        (v as Map).cast<String, dynamic>(),
    ];
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const _Head(
          'Analytics',
          subtitle: 'YouTube video id per published clip → views, likes, and rubric weights that match what performed.',
        ),
        for (final c in clips)
          Padding(
            padding: const EdgeInsets.only(bottom: 6),
            child: TextField(
              controller: _ids.putIfAbsent(c.id, TextEditingController.new),
              decoration: InputDecoration(
                labelText: c.title,
                hintText: 'YouTube video id',
                isDense: true,
                border: const OutlineInputBorder(),
              ),
            ),
          ),
        FilledButton.icon(
          onPressed: _busy
              ? null
              : () async {
                  final pairs = [
                    for (final e in _ids.entries)
                      if (e.value.text.trim().isNotEmpty)
                        [e.key, e.value.text.trim()],
                  ];
                  setState(() => _busy = true);
                  final r = await guarded(
                    context,
                    () => studioCall('analytics', {
                      'id': widget.projectId,
                      'pairs': pairs,
                    }),
                  );
                  if (mounted) {
                    setState(() {
                      _busy = false;
                      if (r is Map) _out = r.cast<String, dynamic>();
                    });
                  }
                },
          icon: const Icon(Icons.insights_outlined),
          label: Text(_busy ? 'Fetching…' : 'Fetch stats'),
        ),
        for (final v in videos)
          ListTile(
            dense: true,
            title: Text(v['title']?.toString() ?? ''),
            subtitle: Text(
              '${v['views']} views · ${v['likes']} likes · ${v['comments']} comments',
            ),
          ),
        if (weights.isNotEmpty) ...[
          const _Head('Suggested rubric weights'),
          for (final e in weights.entries)
            Text('${e.key}: ${((e.value as num) * 100).toStringAsFixed(1)}%'),
        ],
      ],
    );
  }
}
