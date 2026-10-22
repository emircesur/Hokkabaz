import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'clips_pane.dart';
import 'studio_models.dart';
import 'studio_providers.dart';
import 'studio_transfer.dart';

const _criteriaLabels = {
  'hook': 'Hook',
  'emotion': 'Emotion',
  'novelty': 'Novelty',
  'clarity': 'Clarity',
  'pacing': 'Pacing',
  'shareability': 'Shareability',
  'platform_fit': 'Platform fit',
};

const _platformLabels = {
  'youtube_shorts': 'YouTube Shorts',
  'tiktok': 'TikTok',
  'instagram_reels': 'Reels',
  'x': 'X',
  'linkedin': 'LinkedIn',
};

/// Virality estimate, post copy for each platform, render and export.
class PublishPanel extends ConsumerStatefulWidget {
  const PublishPanel({super.key, required this.projectId});

  final String projectId;

  @override
  ConsumerState<PublishPanel> createState() => _PublishPanelState();
}

class _PublishPanelState extends ConsumerState<PublishPanel> {
  final Set<String> _platforms = {'youtube_shorts', 'tiktok', 'instagram_reels'};
  final _tone = TextEditingController();

  @override
  void dispose() {
    _tone.dispose();
    super.dispose();
  }

  Future<void> _export(StudioController ctl, String clip) async {
    if (studioRemote) {
      await ctl.saveRendered(clip);
      return;
    }
    final dir = await FilePicker.getDirectoryPath(dialogTitle: 'Export folder');
    if (dir == null) return;
    await ctl.export(clip, dir);
  }

  Future<void> _youtube(BuildContext context, StudioController ctl, String clip) async {
    var privacy = 'private';
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => StatefulBuilder(
        builder: (ctx, set) => AlertDialog(
          title: const Text('Upload to YouTube'),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text('Uses the title, description and tags from "Post copy". Private uploads can be reviewed on YouTube before going public.'),
              const SizedBox(height: 12),
              SegmentedButton<String>(
                showSelectedIcon: false,
                segments: const [
                  ButtonSegment(value: 'private', label: Text('Private')),
                  ButtonSegment(value: 'unlisted', label: Text('Unlisted')),
                  ButtonSegment(value: 'public', label: Text('Public')),
                ],
                selected: {privacy},
                onSelectionChanged: (v) => set(() => privacy = v.first),
              ),
            ],
          ),
          actions: [
            TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
            FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Upload')),
          ],
        ),
      ),
    );
    if (ok == true) await ctl.publishYoutube(clip, privacy: privacy);
  }

  @override
  Widget build(BuildContext context) {
    final s = ref.watch(studioControllerProvider(widget.projectId));
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final clip = s.clip;
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    if (clip == null) return const Center(child: Text('Select a clip.'));
    final rendering = s.renderProgress[clip.id];
    final rendered = s.rendered[clip.id];
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        _Card(
          title: 'Virality estimate',
          child: clip.score == null
              ? Text('Not scored yet. Find highlights to score clips.', style: TextStyle(color: hk.textMuted))
              : Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Row(
                      children: [
                        ScoreBadge(score: clip.score!.total, size: 56),
                        const SizedBox(width: HkTokens.s3),
                        Expanded(child: Text(clip.score!.summary, style: text.bodyMedium)),
                      ],
                    ),
                    const SizedBox(height: HkTokens.s2),
                    for (final k in ViralScore.criteriaKeys)
                      Padding(
                        padding: const EdgeInsets.symmetric(vertical: 2),
                        child: Row(
                          children: [
                            SizedBox(width: 100, child: Text(_criteriaLabels[k] ?? k, style: text.bodySmall)),
                            Expanded(child: LinearProgressIndicator(value: (clip.score!.criteria[k] ?? 0) / 100, minHeight: 6, borderRadius: BorderRadius.circular(3))),
                            SizedBox(width: 30, child: Text('${clip.score!.criteria[k] ?? 0}', textAlign: TextAlign.end, style: text.bodySmall)),
                          ],
                        ),
                      ),
                    if (clip.score!.improvements.isNotEmpty) ...[
                      const SizedBox(height: HkTokens.s2),
                      Text('To improve', style: text.labelMedium),
                      for (final i in clip.score!.improvements) Text('• $i', style: text.bodySmall),
                    ],
                    const SizedBox(height: 4),
                    Text('An AI estimate, not a guarantee.', style: text.bodySmall?.copyWith(color: hk.textMuted)),
                  ],
                ),
        ),
        _Card(
          title: 'Render',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  FilledButton.icon(
                    onPressed: rendering != null ? null : () => ctl.render(clip.id),
                    icon: const Icon(Icons.movie_creation_outlined, size: 18),
                    label: Text(rendered == null ? 'Render clip' : 'Render again'),
                  ),
                  const SizedBox(width: HkTokens.s2),
                  if (rendered != null)
                    OutlinedButton.icon(
                      onPressed: () => _export(ctl, clip.id),
                      icon: const Icon(Icons.ios_share, size: 18),
                      label: Text(studioRemote ? 'Save clip' : 'Export package'),
                    ),
                ],
              ),
              const SizedBox(height: HkTokens.s2),
              Wrap(
                spacing: HkTokens.s2,
                runSpacing: HkTokens.s2,
                children: [
                  if (!studioRemote)
                    OutlinedButton.icon(
                      onPressed: s.busy.contains('concat') ? null : () => ctl.concatEdit(clip.id),
                      icon: s.busy.contains('concat')
                          ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                          : const Icon(Icons.edit_note, size: 18),
                      label: const Text('Edit in Concat'),
                    ),
                  OutlinedButton.icon(
                    onPressed: rendering != null ? null : () => ctl.concatRender(clip.id),
                    icon: const Icon(Icons.movie_filter_outlined, size: 18),
                    label: const Text('Render with Concat'),
                  ),
                  if (!studioRemote)
                    TextButton(
                      onPressed: s.busy.contains('concat') ? null : () => ctl.concatEdit(clip.id, open: false, rebuild: true),
                      child: const Text('Reset Concat project'),
                    ),
                ],
              ),
              if (rendered != null) ...[
                const SizedBox(height: HkTokens.s2),
                OutlinedButton.icon(
                  onPressed: s.busy.contains('publish') ? null : () => _youtube(context, ctl, clip.id),
                  icon: s.busy.contains('publish')
                      ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                      : const Icon(Icons.upload, size: 18),
                  label: const Text('Upload to YouTube'),
                ),
              ],
              if (rendering != null) ...[
                const SizedBox(height: HkTokens.s2),
                LinearProgressIndicator(value: rendering == 0 ? null : rendering),
              ],
              if (rendered != null) ...[
                const SizedBox(height: HkTokens.s2),
                SelectableText(rendered, style: text.bodySmall?.copyWith(color: hk.textMuted)),
              ],
            ],
          ),
        ),
        _Card(
          title: 'Post copy',
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Wrap(
                spacing: 6,
                children: [
                  for (final e in _platformLabels.entries)
                    FilterChip(
                      label: Text(e.value),
                      selected: _platforms.contains(e.key),
                      onSelected: (v) => setState(() => v ? _platforms.add(e.key) : _platforms.remove(e.key)),
                    ),
                ],
              ),
              const SizedBox(height: HkTokens.s2),
              TextField(controller: _tone, decoration: const InputDecoration(isDense: true, labelText: 'Tone (optional)', hintText: 'playful, professional…', border: OutlineInputBorder())),
              const SizedBox(height: HkTokens.s2),
              FilledButton.tonalIcon(
                onPressed: s.busy.contains('social') || _platforms.isEmpty
                    ? null
                    : () => ctl.social(clip.id, platforms: _platforms.toList(), tone: _tone.text.trim().isEmpty ? null : _tone.text.trim()),
                icon: s.busy.contains('social')
                    ? const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                    : const Icon(Icons.edit_note, size: 20),
                label: const Text('Write titles, descriptions & tags'),
              ),
              if (clip.social != null)
                for (final e in clip.social!.entries) _PostCard(platform: _platformLabels[e.key] ?? e.key, post: (e.value as Map).cast()),
            ],
          ),
        ),
      ],
    );
  }
}

class _Card extends StatelessWidget {
  const _Card({required this.title, required this.child});
  final String title;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      margin: const EdgeInsets.only(bottom: HkTokens.s3),
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(color: hk.surface, border: Border.all(color: hk.border), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [Text(title, style: Theme.of(context).textTheme.titleSmall), const SizedBox(height: HkTokens.s2), child]),
    );
  }
}

class _PostCard extends StatelessWidget {
  const _PostCard({required this.platform, required this.post});

  final String platform;
  final Map<String, dynamic> post;

  String get _full {
    final title = (post['title'] as String?) ?? '';
    final desc = (post['description'] as String?) ?? '';
    final tags = ((post['hashtags'] as List?) ?? const []).join(' ');
    return [if (title.isNotEmpty) title, desc, if (tags.isNotEmpty) tags].join('\n\n');
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      margin: const EdgeInsets.only(top: HkTokens.s2),
      padding: const EdgeInsets.all(HkTokens.s2),
      decoration: BoxDecoration(border: Border.all(color: hk.border), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(child: Text(platform, style: Theme.of(context).textTheme.labelLarge)),
              TextButton.icon(
                onPressed: () async {
                  await Clipboard.setData(ClipboardData(text: _full));
                  if (context.mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$platform copy copied')));
                },
                icon: const Icon(Icons.copy, size: 16),
                label: const Text('Copy'),
              ),
            ],
          ),
          SelectableText(_full, style: Theme.of(context).textTheme.bodySmall),
        ],
      ),
    );
  }
}
