import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'agent_panel.dart';
import 'clips_pane.dart';
import 'player_backends.dart';
import 'player_controller.dart';
import 'publish_panel.dart';
import 'source_strip.dart';
import '../../app/activity.dart';
import 'studio_ai_panels.dart';
import 'studio_home.dart';
import 'studio_timeline.dart';
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
    if (id == null) return const StudioHome();
    return ProjectView(key: ValueKey(id), projectId: id);
  }
}

/// One project: clips, live preview, transcript editor, style, publish, agent.
class ProjectView extends ConsumerStatefulWidget {
  const ProjectView({
    super.key,
    required this.projectId,
    this.backendFactory = createBackend,
  });

  final String projectId;

  /// Overridable for tests.
  final PlayerBackend Function() backendFactory;

  @override
  ConsumerState<ProjectView> createState() => _ProjectViewState();
}

class _ProjectViewState extends ConsumerState<ProjectView> {
  late final EdlPlayerController _player = EdlPlayerController(
    widget.backendFactory(),
  );
  String? _opened;
  String? _playerError;
  String _tool = 'clips';

  @override
  void initState() {
    super.initState();
    // Opened from a home-screen shortcut: start on that tool.
    final pending = ref.read(studioPendingToolProvider);
    if (pending != null) {
      _tool = pending;
      Future.microtask(
        () => ref.read(studioPendingToolProvider.notifier).set(null),
      );
    }
  }

  Widget _panel(String id) => switch (_tool) {
    'enhance' => AiEnhancePanel(projectId: id),
    'captions' => StylePanel(projectId: id),
    'brand' => BrandPanel(projectId: id),
    'broll' => _BrollPanel(projectId: id),
    'motion' => _MotionPanel(projectId: id),
    'text' => WriteupPanel(projectId: id),
    'thumbs' => ThumbnailsPanel(projectId: id),
    'publish' => PublishPanel(projectId: id),
    'stats' => AnalyticsPanel(projectId: id),
    'export' => ExportPanel(projectId: id),
    'agent' => AgentPanel(projectId: id),
    _ => ClipsPane(projectId: id),
  };

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
      final playable = studioRemote
          ? await studioProxyFor(widget.projectId)
          : path;
      await _player.backend.open(await studioMediaFor(playable));
      if (mounted) setState(() => _playerError = null);
      _player.setPlan(
        ref.read(studioControllerProvider(widget.projectId)).plan,
      );
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
      if (next.plan != prev?.plan && _playerError == null) {
        _player.setPlan(next.plan);
      }
    });
    // A Studio job of this project finished (maybe started before this screen opened): reload.
    ref.listen(jobsProvider, (prev, next) {
      for (final j in next.values) {
        final before = prev?[j.id];
        if (before != null &&
            before.running &&
            !j.running &&
            j.meta['project'] == id) {
          ref.read(studioControllerProvider(id).notifier).reload();
          break;
        }
      }
    });
    final s = ref.watch(studioControllerProvider(id));
    final ctl = ref.read(studioControllerProvider(id).notifier);
    final project = s.project;
    final hk = context.hk;
    if (s.loading && project == null) {
      return const Center(child: CircularProgressIndicator());
    }
    if (project == null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(s.notice ?? 'Could not open this project'),
            TextButton(
              onPressed: () =>
                  ref.read(selectedStudioProjectProvider.notifier).select(null),
              child: const Text('Back'),
            ),
          ],
        ),
      );
    }
    final wide = MediaQuery.sizeOf(context).width >= 1000;
    final header = _Header(projectId: id, project: project, state: s, ctl: ctl);
    final center = _Center(
      projectId: id,
      player: _player,
      state: s,
      source: project.source,
      playerError: _playerError,
    );
    return Column(
      children: [
        header,
        if (s.notice != null)
          MaterialBanner(
            content: Text(s.notice!),
            dividerColor: Colors.transparent,
            actions: [
              TextButton(
                onPressed: ctl.clearNotice,
                child: const Text('Dismiss'),
              ),
            ],
          ),
        Divider(height: 1, color: hk.border),
        Expanded(
          child: wide
              ? Column(
                  children: [
                    Expanded(
                      child: Row(
                        children: [
                          SizedBox(
                            width: 380,
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.stretch,
                              children: [
                                Padding(
                                  padding: const EdgeInsets.all(HkTokens.s2),
                                  child: Row(
                                    children: [
                                      FilledButton.tonalIcon(
                                        onPressed: () =>
                                            setState(() => _tool = 'enhance'),
                                        icon: const Icon(
                                          Icons.auto_fix_high,
                                          size: 18,
                                        ),
                                        label: const Text('Speech cleanup'),
                                      ),
                                      const Spacer(),
                                      if (s.clip != null)
                                        Flexible(
                                          child: Text(
                                            s.clip!.title,
                                            maxLines: 1,
                                            overflow: TextOverflow.ellipsis,
                                            style: TextStyle(
                                              color: hk.textMuted,
                                            ),
                                          ),
                                        ),
                                    ],
                                  ),
                                ),
                                Expanded(
                                  child: TranscriptEditor(
                                    projectId: id,
                                    player: _player,
                                  ),
                                ),
                              ],
                            ),
                          ),
                          VerticalDivider(width: 1, color: hk.border),
                          Expanded(
                            child: Column(
                              children: [
                                _PreviewToolbar(projectId: id),
                                Expanded(
                                  child: Padding(
                                    padding: const EdgeInsets.all(HkTokens.s2),
                                    child: s.clip == null
                                        ? Center(
                                            child: FilledButton.icon(
                                              onPressed: ctl.addWholeVideo,
                                              icon: const Icon(
                                                Icons.movie_edit,
                                              ),
                                              label: const Text(
                                                'Edit the whole video',
                                              ),
                                            ),
                                          )
                                        : _playerError != null
                                        ? Center(
                                            child: Text(
                                              _playerError!,
                                              textAlign: TextAlign.center,
                                            ),
                                          )
                                        : ListenableBuilder(
                                            listenable: _player,
                                            builder: (context, _) =>
                                                VideoPreview(
                                                  controller: _player,
                                                  plan: s.plan,
                                                  source: project.source,
                                                ),
                                          ),
                                  ),
                                ),
                                TransportBar(controller: _player),
                              ],
                            ),
                          ),
                          VerticalDivider(width: 1, color: hk.border),
                          SizedBox(
                            width: 340,
                            child: KeyedSubtree(
                              key: ValueKey(_tool),
                              child: _panel(id),
                            ),
                          ),
                          VerticalDivider(width: 1, color: hk.border),
                          _ToolRail(
                            selected: _tool,
                            onSelect: (t) => setState(() => _tool = t),
                          ),
                        ],
                      ),
                    ),
                    Divider(height: 1, color: hk.border),
                    if (s.clip != null)
                      Padding(
                        padding: const EdgeInsets.symmetric(
                          horizontal: HkTokens.s3,
                        ),
                        child: MultiTrackTimeline(
                          projectId: id,
                          player: _player,
                        ),
                      ),
                    Padding(
                      padding: const EdgeInsets.symmetric(
                        horizontal: HkTokens.s3,
                        vertical: HkTokens.s2,
                      ),
                      child: _StripHost(
                        player: _player,
                        plan: s.plan,
                        source: project.source,
                        clips: project.clips,
                        selected: s.selectedClip,
                        onSelect: ctl.selectClip,
                        onBounds: (cid, a, b) => ctl.edit([
                          {
                            'op': 'set_bounds',
                            'clip': cid,
                            'start': a,
                            'end': b,
                          },
                        ]),
                        onAddAt: (at) => ctl.edit([
                          {
                            'op': 'add_clip',
                            'title': 'New clip',
                            'start': at,
                            'end': (at + 30).clamp(0, project.source.duration),
                          },
                        ]),
                      ),
                    ),
                  ],
                )
              : DefaultTabController(
                  length: 10,
                  child: Column(
                    children: [
                      const TabBar(
                        isScrollable: true,
                        tabAlignment: TabAlignment.start,
                        tabs: [
                          Tab(
                            icon: Icon(Icons.video_library_outlined),
                            text: 'Clips',
                          ),
                          Tab(icon: Icon(Icons.edit_outlined), text: 'Edit'),
                          Tab(
                            icon: Icon(Icons.palette_outlined),
                            text: 'Style',
                          ),
                          Tab(icon: Icon(Icons.ios_share), text: 'Publish'),
                          Tab(
                            icon: Icon(Icons.smart_toy_outlined),
                            text: 'Agent',
                          ),
                          Tab(icon: Icon(Icons.auto_fix_high), text: 'Enhance'),
                          Tab(icon: Icon(Icons.article_outlined), text: 'Text'),
                          Tab(
                            icon: Icon(Icons.verified_outlined),
                            text: 'Brand',
                          ),
                          Tab(
                            icon: Icon(Icons.photo_library_outlined),
                            text: 'Thumbs',
                          ),
                          Tab(
                            icon: Icon(Icons.file_download_outlined),
                            text: 'Export',
                          ),
                        ],
                      ),
                      Expanded(
                        child: TabBarView(
                          physics: const NeverScrollableScrollPhysics(),
                          children: [
                            Builder(
                              builder: (ctx) => ClipsPane(
                                projectId: id,
                                onPicked: () =>
                                    DefaultTabController.of(ctx).animateTo(1),
                              ),
                            ),
                            center,
                            StylePanel(projectId: id),
                            PublishPanel(projectId: id),
                            AgentPanel(projectId: id),
                            AiEnhancePanel(projectId: id),
                            WriteupPanel(projectId: id),
                            BrandPanel(projectId: id),
                            ThumbnailsPanel(projectId: id),
                            ExportPanel(projectId: id),
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
  const _Header({
    required this.projectId,
    required this.project,
    required this.state,
    required this.ctl,
  });

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
          IconButton(
            tooltip: 'All projects',
            onPressed: () =>
                ref.read(selectedStudioProjectProvider.notifier).select(null),
            icon: const Icon(Icons.arrow_back),
          ),
          Expanded(
            child: Text(
              project.name,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: Theme.of(context).textTheme.titleMedium,
            ),
          ),
          if (state.lastEditBy == 'agent')
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: Chip(
                visualDensity: VisualDensity.compact,
                avatar: Icon(
                  Icons.smart_toy_outlined,
                  size: 14,
                  color: hk.accent2,
                ),
                label: const Text('Agent edited'),
              ),
            ),
          // Long work runs as core jobs: progress, ETA and Stop survive leaving this screen.
          for (final j in studioJobs(ref, projectId)) _JobChip(job: j),
          // Fallback for work without a job (remote instance).
          if (studioJobs(ref, projectId).isEmpty)
            for (final b in state.busy)
              Padding(
                padding: const EdgeInsets.only(right: 8),
                child: Chip(
                  visualDensity: VisualDensity.compact,
                  avatar: const SizedBox(
                    width: 14,
                    height: 14,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  ),
                  label: Text(b),
                ),
              ),
          IconButton(
            tooltip: 'Undo (Ctrl+Z)',
            onPressed: state.canUndo ? ctl.undo : null,
            icon: const Icon(Icons.undo),
          ),
          IconButton(
            tooltip: 'Redo (Ctrl+Shift+Z)',
            onPressed: state.canRedo ? ctl.redo : null,
            icon: const Icon(Icons.redo),
          ),
          IconButton(
            tooltip: 'Studio settings',
            onPressed: () => showStudioSettings(context),
            icon: const Icon(Icons.tune),
          ),
        ],
      ),
    );
  }
}

class _Center extends ConsumerWidget {
  const _Center({
    required this.projectId,
    required this.player,
    required this.state,
    required this.source,
    required this.playerError,
  });

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
                ? Center(
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Text(
                          'Pick or create a clip',
                          style: TextStyle(color: hk.textMuted),
                        ),
                        const SizedBox(height: HkTokens.s2),
                        FilledButton.icon(
                          onPressed: ctl.addWholeVideo,
                          icon: const Icon(Icons.movie_edit),
                          label: const Text('Edit the whole video'),
                        ),
                      ],
                    ),
                  )
                : playerError != null
                ? Center(
                    child: Padding(
                      padding: const EdgeInsets.all(HkTokens.s4),
                      child: Text(playerError!, textAlign: TextAlign.center),
                    ),
                  )
                : ListenableBuilder(
                    listenable: player,
                    builder: (context, _) => VideoPreview(
                      controller: player,
                      plan: state.plan,
                      source: source,
                    ),
                  ),
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
                {
                  'op': 'add_clip',
                  'title': 'New clip',
                  'start': at,
                  'end': (at + 30).clamp(0, source.duration),
                },
              ]);
              if (ok) {
                final last = ref
                    .read(studioControllerProvider(projectId))
                    .project
                    ?.clips
                    .lastOrNull;
                if (last != null) await ctl.selectClip(last.id);
              }
            },
          ),
        ),
        const Divider(height: 1),
        Expanded(
          flex: 4,
          child: TranscriptEditor(projectId: projectId, player: player),
        ),
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
    _playhead.value = p == null
        ? 0
        : p.clock.toSource(widget.player.outTime.value);
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

/// Right-hand tool rail (OpusClip-style): picks the panel next to it.
class _ToolRail extends StatelessWidget {
  const _ToolRail({required this.selected, required this.onSelect});
  final String selected;
  final void Function(String) onSelect;

  static const _items = <(String, String, IconData)>[
    ('clips', 'Clips', Icons.video_library_outlined),
    ('enhance', 'AI enhance', Icons.auto_awesome),
    ('captions', 'Captions', Icons.closed_caption_outlined),
    ('brand', 'Brand', Icons.verified_outlined),
    ('broll', 'B-roll', Icons.collections_outlined),
    ('motion', 'Motion', Icons.animation),
    ('text', 'Text', Icons.text_fields),
    ('thumbs', 'Thumbs', Icons.photo_library_outlined),
    ('publish', 'Publish', Icons.ios_share),
    ('stats', 'Analytics', Icons.insights_outlined),
    ('export', 'Export', Icons.file_download_outlined),
    ('agent', 'Agent', Icons.smart_toy_outlined),
  ];

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return SizedBox(
      width: 84,
      child: ListView(
        padding: const EdgeInsets.symmetric(vertical: HkTokens.s2),
        children: [
          for (final (k, label, icon) in _items)
            InkWell(
              onTap: () => onSelect(k),
              child: Container(
                margin: const EdgeInsets.symmetric(horizontal: 6, vertical: 3),
                padding: const EdgeInsets.symmetric(vertical: 8),
                decoration: BoxDecoration(
                  color: selected == k
                      ? hk.accent.withValues(alpha: 0.15)
                      : null,
                  borderRadius: BorderRadius.circular(10),
                ),
                child: Column(
                  children: [
                    Icon(icon, color: selected == k ? hk.accent : hk.text),
                    const SizedBox(height: 4),
                    Text(
                      label,
                      textAlign: TextAlign.center,
                      style: TextStyle(
                        fontSize: 11,
                        color: selected == k ? hk.accent : hk.textMuted,
                      ),
                    ),
                  ],
                ),
              ),
            ),
        ],
      ),
    );
  }
}

/// Preview toolbar: aspect, template ("layout") and subject tracker.
class _PreviewToolbar extends ConsumerWidget {
  const _PreviewToolbar({required this.projectId});
  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(studioControllerProvider(projectId));
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final clip = s.clip;
    final catalog = ref.watch(studioCatalogProvider).value ?? const {};
    final templates = ((catalog['templates'] as List?) ?? const []).cast<Map>();
    final aspect = switch (clip?.aspectName) {
      'vertical' => '9:16',
      'square' => '1:1',
      null => 'Original',
      _ => 'Custom',
    };
    return SizedBox(
      height: 44,
      child: Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          PopupMenuButton<String>(
            enabled: clip != null,
            tooltip: 'Aspect ratio',
            onSelected: (v) {
              if (clip == null) return;
              if (v == 'original') {
                ctl.edit([
                  {'op': 'set_reframe', 'clip': clip.id, 'reframe': null},
                ]);
              } else {
                ctl.reframe(clip.id, square: v == 'square');
              }
            },
            itemBuilder: (_) => const [
              PopupMenuItem(value: 'vertical', child: Text('9:16 vertical')),
              PopupMenuItem(value: 'square', child: Text('1:1 square')),
              PopupMenuItem(value: 'original', child: Text('Original')),
            ],
            child: Chip(
              avatar: const Icon(Icons.crop_portrait, size: 16),
              label: Text(aspect),
            ),
          ),
          const SizedBox(width: 8),
          PopupMenuButton<String>(
            enabled: clip != null,
            tooltip: 'Layout / template',
            onSelected: (v) {
              if (clip != null) {
                ctl.edit([
                  {'op': 'apply_template', 'clip': clip.id, 'template': v},
                ]);
              }
            },
            itemBuilder: (_) => [
              for (final t in templates)
                PopupMenuItem(
                  value: t['name'] as String,
                  child: Text(t['name'] as String),
                ),
            ],
            child: Chip(
              avatar: const Icon(Icons.dashboard_outlined, size: 16),
              label: Text('Layout: ${clip?.template ?? 'none'}'),
            ),
          ),
          const SizedBox(width: 8),
          ActionChip(
            avatar: const Icon(Icons.center_focus_strong, size: 16),
            label: Text(s.busy.contains('reframe') ? 'Tracking…' : 'Tracker'),
            onPressed: clip == null || s.busy.contains('reframe')
                ? null
                : () => ctl.reframe(clip.id),
          ),
        ],
      ),
    );
  }
}

class _BrollPanel extends ConsumerWidget {
  const _BrollPanel({required this.projectId});
  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final s = ref.watch(studioControllerProvider(projectId));
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final clip = s.clip;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const Text(
          'B-roll',
          style: TextStyle(fontWeight: FontWeight.w600, fontSize: 15),
        ),
        const SizedBox(height: 4),
        Text(
          'Stock footage at the sentences that need it (Pexels, Pixabay or your folders — set in Studio settings).',
          style: TextStyle(fontSize: 12, color: context.hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s3),
        if (clip == null) const Text('Select a clip.'),
        if (clip != null) ...[
          Text('${clip.brollCount} overlays on this clip'),
          const SizedBox(height: 8),
          FilledButton.icon(
            onPressed: s.busy.contains('broll')
                ? null
                : () => ctl.broll(clip.id),
            icon: const Icon(Icons.auto_awesome),
            label: Text(
              s.busy.contains('broll') ? 'Finding footage…' : 'Auto B-roll',
            ),
          ),
          TextButton(
            onPressed: clip.brollCount == 0
                ? null
                : () => ctl.edit([
                    {'op': 'set_broll', 'clip': clip.id, 'broll': <Object?>[]},
                  ]),
            child: const Text('Remove all B-roll'),
          ),
        ],
      ],
    );
  }
}

class _MotionPanel extends ConsumerStatefulWidget {
  const _MotionPanel({required this.projectId});
  final String projectId;
  @override
  ConsumerState<_MotionPanel> createState() => _MotionPanelState();
}

class _MotionPanelState extends ConsumerState<_MotionPanel> {
  double _zoom = 1.15;
  double _hold = 0.8;

  @override
  Widget build(BuildContext context) {
    final s = ref.watch(studioControllerProvider(widget.projectId));
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final clip = s.clip;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        const Text(
          'Motion',
          style: TextStyle(fontWeight: FontWeight.w600, fontSize: 15),
        ),
        const SizedBox(height: 4),
        Text(
          'Follow the speaker and punch in on emphasised words.',
          style: TextStyle(fontSize: 12, color: context.hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s3),
        if (clip == null) const Text('Select a clip.'),
        if (clip != null) ...[
          OutlinedButton.icon(
            onPressed: () => ctl.reframe(clip.id),
            icon: const Icon(Icons.center_focus_strong),
            label: const Text('Track speaker (9:16)'),
          ),
          const SizedBox(height: HkTokens.s3),
          Text('Punch-in zoom ${_zoom.toStringAsFixed(2)}×'),
          Slider(
            value: _zoom,
            min: 1.05,
            max: 1.4,
            onChanged: (v) => setState(() => _zoom = v),
          ),
          Text('Hold ${_hold.toStringAsFixed(1)} s'),
          Slider(
            value: _hold,
            min: 0.3,
            max: 2.0,
            onChanged: (v) => setState(() => _hold = v),
          ),
          Row(
            children: [
              FilledButton(
                onPressed: () async {
                  await studioCall('set_punch_in', {
                    'id': widget.projectId,
                    'clip': clip.id,
                    'punch_in': {
                      'zoom': _zoom,
                      'hold': _hold,
                      'max_per_minute': 6,
                    },
                  });
                  await ctl.reload();
                },
                child: const Text('Apply auto zoom'),
              ),
              const SizedBox(width: 8),
              TextButton(
                onPressed: () async {
                  await studioCall('set_punch_in', {
                    'id': widget.projectId,
                    'clip': clip.id,
                    'punch_in': null,
                  });
                  await ctl.reload();
                },
                child: const Text('Off'),
              ),
            ],
          ),
          if (clip.reframe == null)
            Text(
              'Auto zoom needs a reframe (Track speaker) first.',
              style: TextStyle(fontSize: 12, color: context.hk.warn),
            ),
        ],
      ],
    );
  }
}

/// Running jobs of one Studio project (transcribe, highlights, renders …), oldest first.
List<Job> studioJobs(WidgetRef ref, String projectId) =>
    ref
        .watch(jobsProvider)
        .values
        .where(
          (j) =>
              j.running && j.parent == null && j.meta['project'] == projectId,
        )
        .toList()
      ..sort((a, b) => a.id.compareTo(b.id));

/// Progress chip with percent, phase, ETA and a Stop button.
class _JobChip extends ConsumerWidget {
  const _JobChip({required this.job});
  final Job job;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final p = job.progress;
    final eta = job.etaMs == null
        ? ''
        : ' · ${(job.etaMs! / 1000).round()}s left';
    final elapsed =
        ((DateTime.now().millisecondsSinceEpoch - job.startedMs) / 1000)
            .round();
    return Container(
      margin: const EdgeInsets.only(right: 8),
      padding: const EdgeInsets.fromLTRB(10, 4, 2, 4),
      decoration: BoxDecoration(
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(20),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          SizedBox(
            width: 70,
            child: LinearProgressIndicator(value: p, minHeight: 4),
          ),
          const SizedBox(width: 8),
          ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 280),
            child: Text(
              '${job.title}${job.phase != null ? ' · ${job.phase}' : ''} · ${p == null ? '${elapsed}s' : '${(p * 100).round()}%'}$eta',
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontSize: 12),
            ),
          ),
          IconButton(
            tooltip: 'Stop',
            visualDensity: VisualDensity.compact,
            icon: Icon(Icons.stop_circle_outlined, size: 18, color: hk.err),
            onPressed: job.state == 'cancelling'
                ? null
                : () => ref.read(jobsProvider.notifier).cancel(job.id),
          ),
        ],
      ),
    );
  }
}
