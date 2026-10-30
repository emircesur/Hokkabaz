import 'dart:io' show File, Platform;

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../../ffi/hk_gate.dart';
import '../../widgets/hk_ui.dart';
import '../ai/ai_common.dart';
import '../chat/chat_providers.dart' show describeError;
import 'studio_ai_panels.dart';
import 'studio_models.dart';
import 'studio_providers.dart';
import 'studio_settings.dart';
import 'studio_transfer.dart';
import 'tools_panel.dart';

/// Studio home: one-click tools, viral presets and recent projects.
class StudioHome extends ConsumerStatefulWidget {
  const StudioHome({super.key});
  @override
  ConsumerState<StudioHome> createState() => _StudioHomeState();
}

class _StudioHomeState extends ConsumerState<StudioHome> {
  String? _preset;
  String? _working;

  static const _tools = <(String, String, IconData)>[
    ('shorts', 'Long to shorts', Icons.view_carousel_outlined),
    ('edit', 'Edit video', Icons.content_cut),
    ('captions', 'AI Captions', Icons.closed_caption_outlined),
    ('broll', 'AI B-roll', Icons.collections_outlined),
    ('reframe', 'AI Reframe', Icons.crop_portrait),
    ('enhance', 'Enhance speech', Icons.graphic_eq),
    ('dub', 'Voice dubbing', Icons.translate),
    ('hook', 'Voiceover hook', Icons.bolt_outlined),
    ('writeup', 'Chapters & blog', Icons.article_outlined),
    ('thumbs', 'Thumbnails', Icons.photo_library_outlined),
    ('brand', 'Brand kit', Icons.verified_outlined),
    ('batch', 'Batch folder', Icons.drive_folder_upload_outlined),
  ];

  static const _toolHelp = <String, String>{
    'edit': 'Cut by editing the transcript',
    'captions': 'Animated, styled subtitles',
    'broll': 'Stock footage where it helps',
    'reframe': 'Track the speaker into 9:16',
    'enhance': 'Clean up voice and silences',
    'dub': 'Translate and re-voice',
    'hook': 'AI hook for the first seconds',
    'writeup': 'YouTube chapters, blog post',
    'thumbs': 'Clickable cover images',
    'brand': 'Logo, colours, intro/outro',
    'batch': 'Clip a whole folder at once',
  };

  Future<String?> _importVideo() async {
    final files = await FilePicker.pickFiles(
      type: FileType.video,
      dialogTitle: 'Choose a video',
    );
    final path = files.firstOrNull?.path;
    if (path == null || !mounted) return null;
    final list = ref.read(studioListProvider.notifier);
    return studioRemote
        ? await withUploadDialog(
            context,
            'Uploading to your instance',
            (p) => list.import(path, onProgress: p),
          )
        : await list.import(path);
  }

  Future<void> _tool(String tool) async {
    if (tool == 'batch') {
      final d = await FilePicker.getDirectoryPath(
        dialogTitle: 'Folder with videos',
      );
      if (d == null || !mounted) return;
      await guarded(
        context,
        () => studioCall('batch', {
          'folder': d,
          'top': 3,
          'template': _preset,
          'opts': ref.read(renderPrefsProvider),
        }),
        ok: 'Batch started — follow it in Activity',
      );
      return;
    }
    try {
      final id = await _importVideo();
      if (id == null || !mounted) return;
      if (tool == 'shorts' || tool == 'captions') {
        setState(
          () => _working = tool == 'shorts'
              ? 'Transcribing and finding highlights…'
              : 'Transcribing…',
        );
        await api.studioTranscribe(id: id);
        if (tool == 'shorts') {
          await api.studioAnalyze(
            id: id,
            count: 8,
            minSecs: 15,
            maxSecs: 75,
            template: _preset,
          );
        }
      }
      if (!mounted) return;
      setState(() => _working = null);
      ref.read(studioPendingToolProvider.notifier).set(switch (tool) {
        'captions' => 'captions',
        'broll' => 'broll',
        'reframe' => 'clips',
        'enhance' || 'dub' => 'enhance',
        'hook' || 'writeup' => 'text',
        'thumbs' => 'thumbs',
        'brand' => 'brand',
        _ => null,
      });
      ref.read(selectedStudioProjectProvider.notifier).select(id);
    } catch (e) {
      if (mounted) {
        setState(() => _working = null);
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text(describeError(e))));
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final list = ref.watch(studioListProvider);
    final tools = ref.watch(studioToolsProvider).value;
    final catalog = ref.watch(studioCatalogProvider).value ?? const {};
    final templates = ((catalog['templates'] as List?) ?? const []).cast<Map>();
    return ListView(
      padding: const EdgeInsets.symmetric(
        horizontal: HkTokens.s6,
        vertical: HkTokens.s4,
      ),
      children: [
        Row(
          children: [
            Expanded(
              child: Text(
                'Studio',
                style: Theme.of(context).textTheme.headlineSmall,
              ),
            ),
            IconButton(
              tooltip: 'Studio settings',
              onPressed: () => showStudioSettings(context),
              icon: const Icon(Icons.tune),
            ),
          ],
        ),
        const SizedBox(height: HkTokens.s4),
        // Hero: import a video.
        Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 900),
            child: Container(
              padding: const EdgeInsets.all(HkTokens.s3),
              decoration: BoxDecoration(
                color: hk.surface,
                borderRadius: BorderRadius.circular(40),
                border: Border.all(color: hk.border),
              ),
              child: Row(
                children: [
                  const SizedBox(width: HkTokens.s3),
                  Icon(Icons.link, color: hk.textMuted),
                  const SizedBox(width: HkTokens.s3),
                  Expanded(
                    child: Text(
                      _working ??
                          (_preset == null
                              ? 'Drop in a long video — get viral clips in one click'
                              : 'Preset: $_preset'),
                      style: TextStyle(color: hk.textMuted, fontSize: 16),
                    ),
                  ),
                  if (_working != null)
                    const Padding(
                      padding: EdgeInsets.all(8),
                      child: SizedBox(
                        width: 18,
                        height: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      ),
                    ),
                  GradientButton(
                    label: 'Get clips in 1 click',
                    icon: Icons.auto_awesome,
                    height: 46,
                    onPressed: _working != null ? null : () => _tool('shorts'),
                  ),
                ],
              ),
            ),
          ),
        ),
        const SizedBox(height: HkTokens.s3),
        const SizedBox(height: HkTokens.s4),
        Wrap(
          spacing: HkTokens.s3,
          runSpacing: HkTokens.s3,
          children: [
            for (final (k, label, icon) in _tools.skip(1))
              SizedBox(
                width: 232,
                child: HoverCard(
                  padding: const EdgeInsets.all(HkTokens.s3),
                  onTap: _working != null ? null : () => _tool(k),
                  child: Row(
                    children: [
                      Container(
                        width: 36,
                        height: 36,
                        decoration: BoxDecoration(
                          color: hk.accent.withValues(alpha: 0.14),
                          borderRadius: BorderRadius.circular(10),
                        ),
                        child: Icon(icon, size: 19, color: hk.accent),
                      ),
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
                              _toolHelp[k] ?? '',
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: 11,
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
        if (tools != null && tools['ffmpeg'] != true)
          Padding(
            padding: const EdgeInsets.only(top: HkTokens.s3),
            child: Platform.isAndroid && !studioRemote
                ? Container(
                    padding: const EdgeInsets.all(HkTokens.s3),
                    decoration: BoxDecoration(
                      color: hk.warn.withValues(alpha: 0.15),
                      borderRadius: BorderRadius.circular(HkTokens.radiusCard),
                    ),
                    child: const Text(
                      'Video Studio needs ffmpeg, which Android cannot run. Connect to a Hokkabaz instance (Instances tab).',
                    ),
                  )
                : const ToolsPanel(onlyMissing: true),
          ),
        const SizedBox(height: HkTokens.s6),
        Text('Viral presets', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: HkTokens.s2),
        SizedBox(
          height: 150,
          child: ListView(
            scrollDirection: Axis.horizontal,
            children: [
              for (final t in templates)
                _PresetCard(
                  name: t['name'] as String,
                  description: t['description'] as String? ?? '',
                  selected: _preset == t['name'],
                  onTap: () => setState(
                    () => _preset = _preset == t['name']
                        ? null
                        : t['name'] as String,
                  ),
                ),
            ],
          ),
        ),
        const SizedBox(height: HkTokens.s6),
        Text('Recent projects', style: Theme.of(context).textTheme.titleLarge),
        const SizedBox(height: HkTokens.s2),
        list.when(
          loading: () => const Padding(
            padding: EdgeInsets.all(32),
            child: Center(child: CircularProgressIndicator()),
          ),
          error: (e, _) => Text(describeError(e)),
          data: (items) => items.isEmpty
              ? Padding(
                  padding: const EdgeInsets.all(HkTokens.s4),
                  child: Text(
                    'No projects yet',
                    style: TextStyle(color: hk.textMuted),
                  ),
                )
              : Wrap(
                  spacing: HkTokens.s3,
                  runSpacing: HkTokens.s3,
                  children: [for (final p in items) _ProjectCard(p: p)],
                ),
        ),
      ],
    );
  }
}

class _PresetCard extends StatelessWidget {
  const _PresetCard({
    required this.name,
    required this.description,
    required this.selected,
    required this.onTap,
  });
  final String name;
  final String description;
  final bool selected;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final hue = (name.hashCode % 360).abs().toDouble();
    return Padding(
      padding: const EdgeInsets.only(right: HkTokens.s3),
      child: InkWell(
        onTap: onTap,
        borderRadius: BorderRadius.circular(14),
        child: Container(
          width: 120,
          padding: const EdgeInsets.all(HkTokens.s2),
          decoration: BoxDecoration(
            borderRadius: BorderRadius.circular(14),
            border: Border.all(
              color: selected ? hk.accent : hk.border,
              width: selected ? 2 : 1,
            ),
            gradient: LinearGradient(
              begin: Alignment.topLeft,
              end: Alignment.bottomRight,
              colors: [
                HSLColor.fromAHSL(1, hue, 0.55, 0.35).toColor(),
                HSLColor.fromAHSL(1, (hue + 40) % 360, 0.6, 0.18).toColor(),
              ],
            ),
          ),
          child: Tooltip(
            message: description,
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                if (selected)
                  const Icon(Icons.check_circle, color: Colors.white, size: 18),
                const Spacer(),
                Text(
                  name.replaceAll('-', ' '),
                  style: const TextStyle(
                    color: Colors.white,
                    fontWeight: FontWeight.w700,
                    fontSize: 14,
                    shadows: [Shadow(blurRadius: 4)],
                  ),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// First-frame-ish thumbnail of a project (cached by the backend under the project's work dir).
final _projectThumbProvider = FutureProvider.family<String?, String>((
  ref,
  id,
) async {
  if (studioRemote) return null;
  try {
    final v = await hk('studio.thumbnail', {'id': id, 'at': 5.0});
    return v is String && File(v).existsSync() ? v : null;
  } catch (_) {
    return null;
  }
});

class _ProjectCard extends ConsumerWidget {
  const _ProjectCard({required this.p});
  final ProjectSummary p;

  Future<void> _delete(BuildContext context, WidgetRef ref) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Delete project?'),
        content: Text(
          '"${p.name}" with its clips, transcript and renders will be removed. The source video file is kept.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            style: FilledButton.styleFrom(backgroundColor: ctx.hk.err),
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (ok == true) await ref.read(studioListProvider.notifier).remove(p.id);
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final thumb = ref.watch(_projectThumbProvider(p.id)).value;
    return SizedBox(
      width: 280,
      child: HoverCard(
        padding: EdgeInsets.zero,
        onTap: () =>
            ref.read(selectedStudioProjectProvider.notifier).select(p.id),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            ClipRRect(
              borderRadius: const BorderRadius.vertical(
                top: Radius.circular(HkTokens.radiusCard),
              ),
              child: SizedBox(
                height: 150,
                child: Stack(
                  fit: StackFit.expand,
                  children: [
                    if (thumb != null)
                      Image.file(
                        File(thumb),
                        fit: BoxFit.cover,
                        errorBuilder: (_, _, _) => const SizedBox(),
                      )
                    else
                      Container(
                        color: hk.surface3,
                        child: Icon(
                          Icons.movie_outlined,
                          size: 36,
                          color: hk.textFaint,
                        ),
                      ),
                    Positioned(
                      right: 8,
                      bottom: 8,
                      child: Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 6,
                          vertical: 2,
                        ),
                        decoration: BoxDecoration(
                          color: Colors.black.withValues(alpha: 0.7),
                          borderRadius: BorderRadius.circular(4),
                        ),
                        child: Text(
                          formatTime(p.duration),
                          style: const TextStyle(
                            fontSize: 11,
                            color: Colors.white,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                  ],
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.fromLTRB(12, 10, 4, 10),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Text(
                          p.name,
                          maxLines: 1,
                          overflow: TextOverflow.ellipsis,
                          style: const TextStyle(
                            fontSize: 13,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                        const SizedBox(height: 6),
                        Wrap(
                          spacing: 6,
                          runSpacing: 4,
                          children: [
                            Pill(
                              '${p.clips} clip${p.clips == 1 ? '' : 's'}',
                              icon: Icons.video_library_outlined,
                            ),
                            if (p.hasTranscript)
                              Pill(
                                'Transcribed',
                                icon: Icons.check,
                                color: hk.ok,
                              )
                            else
                              Pill(
                                'Not transcribed',
                                icon: Icons.subtitles_off_outlined,
                                color: hk.warn,
                              ),
                          ],
                        ),
                      ],
                    ),
                  ),
                  PopupMenuButton<String>(
                    tooltip: 'More',
                    onSelected: (v) {
                      if (v == 'delete') _delete(context, ref);
                    },
                    itemBuilder: (_) => const [
                      PopupMenuItem(
                        value: 'delete',
                        child: Text('Delete project'),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }
}
