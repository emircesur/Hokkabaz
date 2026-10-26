import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'studio_models.dart';
import 'studio_providers.dart';

/// Clip candidates and edits, best score first.
class ClipsPane extends ConsumerWidget {
  const ClipsPane({super.key, required this.projectId, this.onPicked});

  final String projectId;

  /// Narrow layouts jump to the editor after a pick.
  final VoidCallback? onPicked;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(studioControllerProvider(projectId));
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final project = s.project;
    final hk = context.hk;
    if (project == null) {
      return const Center(child: CircularProgressIndicator());
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.all(HkTokens.s2),
          child: Wrap(
            spacing: HkTokens.s2,
            runSpacing: HkTokens.s2,
            children: [
              FilledButton.icon(
                onPressed: !project.hasTranscript || s.busy.contains('analyze')
                    ? null
                    : () => _analyze(context, ctl),
                icon: s.busy.contains('analyze')
                    ? const SizedBox(
                        width: 16,
                        height: 16,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.auto_awesome, size: 18),
                label: Text(
                  s.busy.contains('analyze') ? 'Finding…' : 'Find highlights',
                ),
              ),
              OutlinedButton.icon(
                onPressed: ctl.addWholeVideo,
                icon: const Icon(Icons.movie_edit, size: 18),
                label: const Text('Whole video'),
              ),
              if (!project.hasTranscript)
                OutlinedButton.icon(
                  onPressed: s.busy.contains('transcribe')
                      ? null
                      : ctl.transcribe,
                  icon: s.busy.contains('transcribe')
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.subtitles_outlined, size: 18),
                  label: Text(
                    s.busy.contains('transcribe')
                        ? 'Transcribing…'
                        : 'Transcribe',
                  ),
                ),
            ],
          ),
        ),
        Divider(height: 1, color: hk.border),
        Expanded(
          child: project.clips.isEmpty
              ? Center(
                  child: Padding(
                    padding: const EdgeInsets.all(HkTokens.s4),
                    child: Text(
                      project.hasTranscript
                          ? 'No clips yet. Edit the whole video, find highlights, or long-press the timeline to add one.'
                          : 'Edit the whole video now, or transcribe it first so the AI can find the best moments and you can edit by text.',
                      textAlign: TextAlign.center,
                      style: TextStyle(color: hk.textMuted),
                    ),
                  ),
                )
              : ListView.builder(
                  itemCount: project.clips.length,
                  itemBuilder: (context, i) => _ClipTile(
                    clip: project.clips[i],
                    selected: project.clips[i].id == s.selectedClip,
                    rendering: s.renderProgress[project.clips[i].id],
                    rendered: s.rendered.containsKey(project.clips[i].id),
                    onTap: () {
                      ctl.selectClip(project.clips[i].id);
                      onPicked?.call();
                    },
                    onRename: (t) => ctl.edit([
                      {
                        'op': 'rename_clip',
                        'clip': project.clips[i].id,
                        'title': t,
                      },
                    ]),
                    onRemove: () => ctl.edit([
                      {'op': 'remove_clip', 'clip': project.clips[i].id},
                    ]),
                  ),
                ),
        ),
      ],
    );
  }

  Future<void> _analyze(BuildContext context, StudioController ctl) async {
    final count = TextEditingController(text: '8');
    final brief = TextEditingController();
    final min = TextEditingController(text: '15');
    final max = TextEditingController(text: '75');
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Find highlights'),
        content: SizedBox(
          width: 360,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextField(
                controller: brief,
                decoration: const InputDecoration(
                  labelText: 'Brief (optional)',
                  hintText: 'e.g. only funny moments',
                ),
              ),
              Row(
                children: [
                  Expanded(
                    child: TextField(
                      controller: count,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: 'Clips'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: TextField(
                      controller: min,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: 'Min s'),
                    ),
                  ),
                  const SizedBox(width: 8),
                  Expanded(
                    child: TextField(
                      controller: max,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: 'Max s'),
                    ),
                  ),
                ],
              ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Find'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    await ctl.analyze(
      count: int.tryParse(count.text) ?? 8,
      minSecs: double.tryParse(min.text) ?? 15,
      maxSecs: double.tryParse(max.text) ?? 75,
      brief: brief.text.trim().isEmpty ? null : brief.text.trim(),
      template: 'shorts-karaoke',
    );
  }
}

class _ClipTile extends StatelessWidget {
  const _ClipTile({
    required this.clip,
    required this.selected,
    required this.rendering,
    required this.rendered,
    required this.onTap,
    required this.onRename,
    required this.onRemove,
  });

  final StudioClip clip;
  final bool selected;
  final double? rendering;
  final bool rendered;
  final VoidCallback onTap;
  final void Function(String) onRename;
  final VoidCallback onRemove;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    return Material(
      color: selected ? hk.accent.withValues(alpha: 0.12) : Colors.transparent,
      child: InkWell(
        onTap: onTap,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: 56),
          child: Padding(
            padding: const EdgeInsets.symmetric(
              horizontal: HkTokens.s3,
              vertical: HkTokens.s2,
            ),
            child: Row(
              children: [
                ScoreBadge(score: clip.score?.total),
                const SizedBox(width: HkTokens.s3),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        clip.title,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.bodyMedium?.copyWith(
                          fontWeight: FontWeight.w600,
                        ),
                      ),
                      Text(
                        '${formatTime(clip.start)} – ${formatTime(clip.end)}  ·  ${clip.duration.toStringAsFixed(0)}s${clip.deletedWords.isEmpty ? '' : '  ·  ${clip.deletedWords.length} cut'}',
                        style: text.bodySmall?.copyWith(color: hk.textMuted),
                      ),
                      if (rendering != null)
                        LinearProgressIndicator(
                          value: rendering == 0 ? null : rendering,
                          minHeight: 3,
                        ),
                    ],
                  ),
                ),
                if (rendered) Icon(Icons.check_circle, size: 16, color: hk.ok),
                PopupMenuButton<String>(
                  tooltip: 'Clip actions',
                  onSelected: (v) async {
                    if (v == 'delete') onRemove();
                    if (v == 'rename') {
                      final c = TextEditingController(text: clip.title);
                      final t = await showDialog<String>(
                        context: context,
                        builder: (ctx) => AlertDialog(
                          title: const Text('Rename clip'),
                          content: TextField(controller: c, autofocus: true),
                          actions: [
                            TextButton(
                              onPressed: () => Navigator.pop(ctx),
                              child: const Text('Cancel'),
                            ),
                            FilledButton(
                              onPressed: () =>
                                  Navigator.pop(ctx, c.text.trim()),
                              child: const Text('Rename'),
                            ),
                          ],
                        ),
                      );
                      if (t != null && t.isNotEmpty) onRename(t);
                    }
                  },
                  itemBuilder: (_) => const [
                    PopupMenuItem(value: 'rename', child: Text('Rename')),
                    PopupMenuItem(value: 'delete', child: Text('Delete clip')),
                  ],
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

/// Virality score as a ring; grey when unscored.
class ScoreBadge extends StatelessWidget {
  const ScoreBadge({super.key, required this.score, this.size = 40});

  final int? score;
  final double size;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final s = score;
    final color = s == null
        ? hk.textMuted
        : s >= 75
        ? hk.ok
        : s >= 50
        ? hk.warn
        : hk.err;
    return Tooltip(
      message: s == null ? 'Not scored' : 'Virality estimate $s/100',
      child: SizedBox(
        width: size,
        height: size,
        child: Stack(
          alignment: Alignment.center,
          children: [
            CircularProgressIndicator(
              value: s == null ? 0 : s / 100,
              strokeWidth: 3.5,
              color: color,
              backgroundColor: hk.border,
            ),
            Text(
              s?.toString() ?? '–',
              style: TextStyle(
                fontSize: size * 0.32,
                fontWeight: FontWeight.w700,
                color: color,
              ),
            ),
          ],
        ),
      ),
    );
  }
}
