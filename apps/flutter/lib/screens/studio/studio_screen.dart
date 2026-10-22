import 'dart:io' show Platform;

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import 'agent_panel.dart';
import 'clips_pane.dart';
import 'player_backends.dart';
import 'player_controller.dart';
import 'publish_panel.dart';
import 'source_strip.dart';
import 'studio_models.dart';
import 'studio_providers.dart';
import 'studio_settings.dart';
import 'studio_transfer.dart';
import 'style_panel.dart';
import 'transcript_editor.dart';
import 'video_preview.dart';

/// Video Studio: long video in, scored vertical clips out, edited by text and by an agent, live.
class StudioScreen extends ConsumerWidget {
  const StudioScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final id = ref.watch(selectedStudioProjectProvider);
    if (id == null) return const _ProjectsView();
    return ProjectView(key: ValueKey(id), projectId: id);
  }
}

class _ProjectsView extends ConsumerWidget {
  const _ProjectsView();

  Future<void> _import(BuildContext context, WidgetRef ref) async {
    final files = await FilePicker.pickFiles(type: FileType.video, dialogTitle: 'Choose a video');
    final path = files.firstOrNull?.path;
    if (path == null) return;
    try {
      final list = ref.read(studioListProvider.notifier);
      final id = studioRemote
          ? await withUploadDialog(context, 'Uploading to your instance', (p) => list.import(path, onProgress: p))
          : await list.import(path);
      ref.read(selectedStudioProjectProvider.notifier).select(id);
    } catch (e) {
      if (context.mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(describeError(e))));
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final list = ref.watch(studioListProvider);
    final tools = ref.watch(studioToolsProvider).value;
    final hk = context.hk;
    return Align(
      alignment: Alignment.topCenter,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 900),
        child: ListView(
          padding: const EdgeInsets.all(HkTokens.s4),
          children: [
            Row(
              children: [
                Expanded(child: Text('Studio', style: Theme.of(context).textTheme.headlineSmall)),
                IconButton(tooltip: 'Studio settings', onPressed: () => showStudioSettings(context), icon: const Icon(Icons.tune)),
                const SizedBox(width: 4),
                FilledButton.icon(onPressed: () => _import(context, ref), icon: const Icon(Icons.video_file_outlined), label: const Text('Import video')),
              ],
            ),
            const SizedBox(height: HkTokens.s1),
            Text('Turn long videos into short, captioned, vertical clips. Edit by text, or let the agent do it.', style: TextStyle(color: hk.textMuted)),
            if (tools != null && tools['ffmpeg'] != true)
              Container(
                margin: const EdgeInsets.only(top: HkTokens.s3),
                padding: const EdgeInsets.all(HkTokens.s3),
                decoration: BoxDecoration(color: hk.warn.withValues(alpha: 0.15), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
                child: Text(Platform.isAndroid ? 'Video Studio needs ffmpeg, which Android does not provide. Projects and the agent tools work on desktop; use the desktop app for editing and rendering.' : 'ffmpeg was not found on this device, so videos cannot be imported or rendered here. Install ffmpeg (or point HK_FFMPEG at it).'),
              ),
            const SizedBox(height: HkTokens.s4),
            list.when(
              loading: () => const Center(child: Padding(padding: EdgeInsets.all(32), child: CircularProgressIndicator())),
              error: (e, _) => Text(describeError(e)),
              data: (items) => items.isEmpty
                  ? Container(
                      padding: const EdgeInsets.all(HkTokens.s6),
                      decoration: BoxDecoration(border: Border.all(color: hk.border), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
                      child: Column(
                        children: [
                          Icon(Icons.movie_creation_outlined, size: 40, color: hk.textMuted),
                          const SizedBox(height: HkTokens.s2),
                          const Text('No projects yet'),
                          const SizedBox(height: HkTokens.s2),
                          OutlinedButton(onPressed: () => showStudioSettings(context), child: const Text('Set up models and keys first')),
                        ],
                      ),
                    )
                  : Column(
                      children: [
                        for (final p in items)
                          Card(
                            margin: const EdgeInsets.only(bottom: HkTokens.s2),
                            child: ListTile(
                              minTileHeight: 64,
                              leading: const Icon(Icons.movie_outlined),
                              title: Text(p.name),
                              subtitle: Text('${formatTime(p.duration)}  ·  ${p.width}×${p.height}  ·  ${p.clips} clips${p.hasTranscript ? '' : '  ·  not transcribed'}'),
                              onTap: () => ref.read(selectedStudioProjectProvider.notifier).select(p.id),
                              trailing: PopupMenuButton<String>(
                                onSelected: (v) async {
                                  if (v == 'delete') {
                                    final ok = await showDialog<bool>(
                                      context: context,
                                      builder: (ctx) => AlertDialog(
                                        title: Text('Delete "${p.name}"?'),
                                        content: const Text('Clips, transcript and renders of this project are removed. The original video file is not touched.'),
                                        actions: [
                                          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
                                          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: const Text('Delete')),
                                        ],
                                      ),
                                    );
                                    if (ok == true) await ref.read(studioListProvider.notifier).remove(p.id);
                                  }
                                },
                                itemBuilder: (_) => const [PopupMenuItem(value: 'delete', child: Text('Delete project'))],
                              ),
                            ),
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

/// One project: clips, live preview, transcript editor, style, publish, agent.
class ProjectView extends ConsumerStatefulWidget {
  const ProjectView({super.key, required this.projectId, this.backendFactory = createBackend});

  final String projectId;

  /// Overridable for tests.
  final PlayerBackend Function() backendFactory;

  @override
  ConsumerState<ProjectView> createState() => _ProjectViewState();
}

class _ProjectViewState extends ConsumerState<ProjectView> {
  late final EdlPlayerController _player = EdlPlayerController(widget.backendFactory());
  String? _opened;
  String? _playerError;

  @override
  void dispose() {
    _player.dispose();
    super.dispose();
  }

  Future<void> _openSource(String path) async {
    if (_opened == path) return;
    _opened = path;
    try {
      // A remote instance streams a small proxy, not the original.
      final playable = studioRemote ? await studioProxyFor(widget.projectId) : path;
      await _player.backend.open(await studioMediaFor(playable));
      if (mounted) setState(() => _playerError = null);
      _player.setPlan(ref.read(studioControllerProvider(widget.projectId)).plan);
    } catch (e) {
      if (mounted) setState(() => _playerError = 'Preview unavailable: $e');
    }
  }

  @override
  Widget build(BuildContext context) {
    final id = widget.projectId;
    ref.listen(studioControllerProvider(id), (prev, next) {
      final path = next.project?.source.path;
      if (path != null) _openSource(path);
      if (next.plan != prev?.plan && _playerError == null) _player.setPlan(next.plan);
    });
    final s = ref.watch(studioControllerProvider(id));
    final ctl = ref.read(studioControllerProvider(id).notifier);
    final project = s.project;
    final hk = context.hk;
    if (s.loading && project == null) return const Center(child: CircularProgressIndicator());
    if (project == null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(s.notice ?? 'Could not open this project'),
            TextButton(onPressed: () => ref.read(selectedStudioProjectProvider.notifier).select(null), child: const Text('Back')),
          ],
        ),
      );
    }
    final wide = MediaQuery.sizeOf(context).width >= 1000;
    final header = _Header(projectId: id, project: project, state: s, ctl: ctl);
    final center = _Center(projectId: id, player: _player, state: s, source: project.source, playerError: _playerError);
    return Column(
      children: [
        header,
        if (s.notice != null)
          MaterialBanner(
            content: Text(s.notice!),
            dividerColor: Colors.transparent,
            actions: [TextButton(onPressed: ctl.clearNotice, child: const Text('Dismiss'))],
          ),
        Divider(height: 1, color: hk.border),
        Expanded(
          child: wide
              ? Row(
                  children: [
                    SizedBox(width: 300, child: ClipsPane(projectId: id)),
                    VerticalDivider(width: 1, color: hk.border),
                    Expanded(child: center),
                    VerticalDivider(width: 1, color: hk.border),
                    SizedBox(width: 380, child: _RightTabs(projectId: id)),
                  ],
                )
              : DefaultTabController(
                  length: 5,
                  child: Column(
                    children: [
                      const TabBar(
                        isScrollable: true,
                        tabAlignment: TabAlignment.start,
                        tabs: [
                          Tab(icon: Icon(Icons.video_library_outlined), text: 'Clips'),
                          Tab(icon: Icon(Icons.edit_outlined), text: 'Edit'),
                          Tab(icon: Icon(Icons.palette_outlined), text: 'Style'),
                          Tab(icon: Icon(Icons.ios_share), text: 'Publish'),
                          Tab(icon: Icon(Icons.smart_toy_outlined), text: 'Agent'),
                        ],
                      ),
                      Expanded(
                        child: TabBarView(
                          physics: const NeverScrollableScrollPhysics(),
                          children: [
                            Builder(builder: (ctx) => ClipsPane(projectId: id, onPicked: () => DefaultTabController.of(ctx).animateTo(1))),
                            center,
                            StylePanel(projectId: id),
                            PublishPanel(projectId: id),
                            AgentPanel(projectId: id),
                          ],
                        ),
                      ),
                    ],
                  ),
                ),
        ),
      ],
    );
  }
}

class _Header extends ConsumerWidget {
  const _Header({required this.projectId, required this.project, required this.state, required this.ctl});

  final String projectId;
  final StudioProjectData project;
  final StudioState state;
  final StudioController ctl;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    return SizedBox(
      height: 52,
      child: Row(
        children: [
          IconButton(tooltip: 'All projects', onPressed: () => ref.read(selectedStudioProjectProvider.notifier).select(null), icon: const Icon(Icons.arrow_back)),
          Expanded(child: Text(project.name, maxLines: 1, overflow: TextOverflow.ellipsis, style: Theme.of(context).textTheme.titleMedium)),
          if (state.lastEditBy == 'agent')
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: Chip(
                visualDensity: VisualDensity.compact,
                avatar: Icon(Icons.smart_toy_outlined, size: 14, color: hk.accent2),
                label: const Text('Agent edited'),
              ),
            ),
          for (final b in state.busy)
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: Chip(visualDensity: VisualDensity.compact, avatar: const SizedBox(width: 14, height: 14, child: CircularProgressIndicator(strokeWidth: 2)), label: Text(b)),
            ),
          IconButton(tooltip: 'Undo (Ctrl+Z)', onPressed: state.canUndo ? ctl.undo : null, icon: const Icon(Icons.undo)),
          IconButton(tooltip: 'Redo (Ctrl+Shift+Z)', onPressed: state.canRedo ? ctl.redo : null, icon: const Icon(Icons.redo)),
          IconButton(tooltip: 'Studio settings', onPressed: () => showStudioSettings(context), icon: const Icon(Icons.tune)),
        ],
      ),
    );
  }
}

class _RightTabs extends StatelessWidget {
  const _RightTabs({required this.projectId});
  final String projectId;

  @override
  Widget build(BuildContext context) => DefaultTabController(
    length: 3,
    child: Column(
      children: [
        const TabBar(tabs: [Tab(text: 'Agent'), Tab(text: 'Style'), Tab(text: 'Publish')]),
        Expanded(
          child: TabBarView(
            physics: const NeverScrollableScrollPhysics(),
            children: [AgentPanel(projectId: projectId), StylePanel(projectId: projectId), PublishPanel(projectId: projectId)],
          ),
        ),
      ],
    ),
  );
}

class _Center extends ConsumerWidget {
  const _Center({required this.projectId, required this.player, required this.state, required this.source, required this.playerError});

  final String projectId;
  final EdlPlayerController player;
  final StudioState state;
  final StudioSource source;
  final String? playerError;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final clip = state.clip;
    final hk = context.hk;
    return Column(
      children: [
        Expanded(
          flex: 5,
          child: Padding(
            padding: const EdgeInsets.all(HkTokens.s2),
            child: clip == null
                ? Center(child: Text('Pick or create a clip', style: TextStyle(color: hk.textMuted)))
                : playerError != null
                ? Center(child: Padding(padding: const EdgeInsets.all(HkTokens.s4), child: Text(playerError!, textAlign: TextAlign.center)))
                : ListenableBuilder(listenable: player, builder: (context, _) => VideoPreview(controller: player, plan: state.plan, source: source)),
          ),
        ),
        TransportBar(controller: player),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
          child: _StripHost(
            player: player,
            plan: state.plan,
            source: source,
            clips: state.project?.clips ?? const [],
            selected: state.selectedClip,
            onSelect: ctl.selectClip,
            onBounds: (id, s, e) => ctl.edit([
              {'op': 'set_bounds', 'clip': id, 'start': s, 'end': e},
            ]),
            onAddAt: (at) async {
              final ok = await ctl.edit([
                {'op': 'add_clip', 'title': 'New clip', 'start': at, 'end': (at + 30).clamp(0, source.duration)},
              ]);
              if (ok) {
                final last = ref.read(studioControllerProvider(projectId)).project?.clips.lastOrNull;
                if (last != null) await ctl.selectClip(last.id);
              }
            },
          ),
        ),
        const Divider(height: 1),
        Expanded(flex: 4, child: TranscriptEditor(projectId: projectId, player: player)),
      ],
    );
  }
}

/// Owns the source-time playhead notifier for [SourceStrip] (output time mapped through the plan).
class _StripHost extends StatefulWidget {
  const _StripHost({
    required this.player,
    required this.plan,
    required this.source,
    required this.clips,
    required this.selected,
    required this.onSelect,
    required this.onBounds,
    required this.onAddAt,
  });

  final EdlPlayerController player;
  final PreviewPlan? plan;
  final StudioSource source;
  final List<StudioClip> clips;
  final String? selected;
  final void Function(String) onSelect;
  final void Function(String, double, double) onBounds;
  final void Function(double) onAddAt;

  @override
  State<_StripHost> createState() => _StripHostState();
}

class _StripHostState extends State<_StripHost> {
  final _playhead = ValueNotifier<double>(0);

  @override
  void initState() {
    super.initState();
    widget.player.outTime.addListener(_sync);
    _sync();
  }

  @override
  void didUpdateWidget(_StripHost old) {
    super.didUpdateWidget(old);
    _sync();
  }

  void _sync() {
    final p = widget.plan;
    _playhead.value = p == null ? 0 : p.clock.toSource(widget.player.outTime.value);
  }

  @override
  void dispose() {
    widget.player.outTime.removeListener(_sync);
    _playhead.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => SourceStrip(
    duration: widget.source.duration,
    clips: widget.clips,
    selected: widget.selected,
    playhead: _playhead,
    onSelect: widget.onSelect,
    onBounds: widget.onBounds,
    onAddAt: widget.onAddAt,
  );
}
