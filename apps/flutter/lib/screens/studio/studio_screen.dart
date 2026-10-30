import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import '../../widgets/hk_ui.dart';
import '../ai/ai_common.dart' show guarded;
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

  /// New 30 s clip at `at` (source seconds), then select it.
  Future<void> _addClipAt(double at) async {
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final dur =
        ref
            .read(studioControllerProvider(widget.projectId))
            .project
            ?.source
            .duration ??
        0;
    final ok = await ctl.edit([
      {
        'op': 'add_clip',
        'title': 'New clip',
        'start': at,
        'end': (at + 30).clamp(0, dur),
      },
    ]);
    if (!ok) return;
    final last = ref
        .read(studioControllerProvider(widget.projectId))
        .project
        ?.clips
        .lastOrNull;
    if (last != null) await ctl.selectClip(last.id);
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

  /// NLE transport keys (P9.28): J back 5 s, K pause, L play (again: forward 5 s),
  /// space toggles, arrows step 1 s. Ignored while a text field has focus.
  KeyEventResult _transportKey(FocusNode node, KeyEvent e) {
    if (e is! KeyDownEvent && e is! KeyRepeatEvent) {
      return KeyEventResult.ignored;
    }
    final focused = FocusManager.instance.primaryFocus?.context;
    if (focused != null &&
        (focused.widget is EditableText ||
            focused.findAncestorWidgetOfExactType<EditableText>() != null)) {
      return KeyEventResult.ignored;
    }
    final p = _player;
    final t = p.outTime.value;
    switch (e.logicalKey) {
      case LogicalKeyboardKey.keyJ:
        p.seekOut(t - 5);
      case LogicalKeyboardKey.keyK:
        p.pause();
      case LogicalKeyboardKey.keyL:
        p.playing ? p.seekOut(t + 5) : p.play();
      case LogicalKeyboardKey.space:
        p.toggle();
      case LogicalKeyboardKey.arrowLeft:
        p.seekOut(t - 1);
      case LogicalKeyboardKey.arrowRight:
        p.seekOut(t + 1);
      default:
        return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
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
    // Re-entering a project whose state is already loaded never fires the listener above.
    final src = project?.source.path;
    if (src != null && _opened != src) {
      Future.microtask(() => _openSource(src));
    }
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
    final header = _Header(
      projectId: id,
      project: project,
      state: s,
      ctl: ctl,
      onExport: wide ? () => setState(() => _tool = 'export') : null,
    );
    final center = _Center(
      projectId: id,
      player: _player,
      state: s,
      source: project.source,
      playerError: _playerError,
    );
    return Focus(
      autofocus: true,
      onKeyEvent: _transportKey,
      child: Column(
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
                                  PanelHeader(
                                    title: s.clip == null
                                        ? 'Transcript'
                                        : 'Transcript · ${s.clip!.title}',
                                    icon: Icons.notes,
                                    actions: [
                                      TextButton.icon(
                                        onPressed: () =>
                                            setState(() => _tool = 'enhance'),
                                        icon: const Icon(
                                          Icons.auto_fix_high,
                                          size: 16,
                                        ),
                                        label: const Text('Clean up'),
                                      ),
                                    ],
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
                                      padding: const EdgeInsets.all(
                                        HkTokens.s2,
                                      ),
                                      child: s.clip == null
                                          ? EmptyState(
                                              icon: Icons.movie_filter_outlined,
                                              title: 'No clip selected',
                                              message: project.hasTranscript
                                                  ? 'Let AI find the best moments, edit the whole video, or double-click the source bar below to add a clip.'
                                                  : 'Transcribe first (Clips panel) so AI can find highlights, or edit the whole video.',
                                              action: Wrap(
                                                spacing: HkTokens.s2,
                                                runSpacing: HkTokens.s2,
                                                alignment: WrapAlignment.center,
                                                children: [
                                                  GradientButton(
                                                    label: 'Find highlights',
                                                    icon: Icons.auto_awesome,
                                                    onPressed: () => setState(
                                                      () => _tool = 'clips',
                                                    ),
                                                  ),
                                                  OutlinedButton.icon(
                                                    onPressed:
                                                        ctl.addWholeVideo,
                                                    icon: const Icon(
                                                      Icons.movie_edit,
                                                      size: 18,
                                                    ),
                                                    label: const Text(
                                                      'Edit whole video',
                                                    ),
                                                  ),
                                                ],
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
                              width: 360,
                              child: ColoredBox(
                                color: hk.surface,
                                child: Column(
                                  crossAxisAlignment:
                                      CrossAxisAlignment.stretch,
                                  children: [
                                    PanelHeader(
                                      title: _ToolRail.label(_tool),
                                      icon: _ToolRail.icon(_tool),
                                    ),
                                    Expanded(
                                      child: KeyedSubtree(
                                        key: ValueKey(_tool),
                                        child: _panel(id),
                                      ),
                                    ),
                                  ],
                                ),
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
                        padding: const EdgeInsets.fromLTRB(
                          HkTokens.s3,
                          HkTokens.s1,
                          HkTokens.s3,
                          HkTokens.s2,
                        ),
                        child: Row(
                          children: [
                            SizedBox(
                              width: 84,
                              child: Row(
                                children: [
                                  Icon(
                                    Icons.movie_outlined,
                                    size: 14,
                                    color: hk.textFaint,
                                  ),
                                  const SizedBox(width: 6),
                                  Flexible(
                                    child: Text(
                                      'Source',
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: TextStyle(
                                        fontSize: 11,
                                        color: hk.textMuted,
                                      ),
                                    ),
                                  ),
                                ],
                              ),
                            ),
                            Expanded(
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
                                onAddAt: _addClipAt,
                              ),
                            ),
                          ],
                        ),
                      ),
                    ],
                  )
                : DefaultTabController(
                    length: 13,
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
                            Tab(
                              icon: Icon(Icons.auto_fix_high),
                              text: 'Enhance',
                            ),
                            Tab(
                              icon: Icon(Icons.article_outlined),
                              text: 'Text',
                            ),
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
                            Tab(
                              icon: Icon(Icons.collections_outlined),
                              text: 'B-roll',
                            ),
                            Tab(icon: Icon(Icons.animation), text: 'Motion'),
                            Tab(
                              icon: Icon(Icons.insights_outlined),
                              text: 'Analytics',
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
                              _BrollPanel(projectId: id),
                              _MotionPanel(projectId: id),
                              AnalyticsPanel(projectId: id),
                            ],
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

class _Header extends ConsumerWidget {
  const _Header({
    required this.projectId,
    required this.project,
    required this.state,
    required this.ctl,
    this.onExport,
  });

  final String projectId;
  final StudioProjectData project;
  final StudioState state;
  final StudioController ctl;
  final VoidCallback? onExport;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final clips = project.clips.length;
    return Container(
      height: 52,
      padding: const EdgeInsets.only(right: HkTokens.s2),
      color: hk.surface,
      child: Row(
        children: [
          IconButton(
            tooltip: 'All projects',
            onPressed: () =>
                ref.read(selectedStudioProjectProvider.notifier).select(null),
            icon: const Icon(Icons.arrow_back),
          ),
          Expanded(
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(
                  project.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(
                    fontSize: 14,
                    fontWeight: FontWeight.w600,
                  ),
                ),
                Text(
                  [
                    formatTime(project.source.duration),
                    '${project.source.width}×${project.source.height}',
                    '$clips clip${clips == 1 ? '' : 's'}',
                    if (!project.hasTranscript) 'not transcribed',
                  ].join('  ·  '),
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: TextStyle(fontSize: 11, color: hk.textFaint),
                ),
              ],
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
            tooltip: 'Keyboard shortcuts',
            onPressed: () => _showShortcuts(context),
            icon: const Icon(Icons.keyboard_outlined),
          ),
          IconButton(
            tooltip: 'Studio settings',
            onPressed: () => showStudioSettings(context),
            icon: const Icon(Icons.tune),
          ),
          if (onExport != null) ...[
            const SizedBox(width: HkTokens.s2),
            GradientButton(
              label: 'Export',
              icon: Icons.file_upload_outlined,
              height: 34,
              onPressed: state.clip == null ? null : onExport,
            ),
          ],
        ],
      ),
    );
  }
}

void _showShortcuts(BuildContext context) {
  const rows = [
    ('Space', 'Play / pause'),
    ('J  /  L', 'Back 5 s  /  play, again: forward 5 s'),
    ('K', 'Pause'),
    ('←  /  →', 'Step 1 s'),
    ('Ctrl+Z  /  Ctrl+Shift+Z', 'Undo / redo'),
    ('Delete', 'Cut selected words (transcript)'),
    ('Double-click source bar', 'Add a clip there'),
  ];
  showDialog<void>(
    context: context,
    builder: (ctx) {
      final hk = ctx.hk;
      return AlertDialog(
        title: const Text('Keyboard shortcuts'),
        content: SizedBox(
          width: 420,
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (final (k, v) in rows)
                Padding(
                  padding: const EdgeInsets.symmetric(vertical: 5),
                  child: Row(
                    children: [
                      Container(
                        padding: const EdgeInsets.symmetric(
                          horizontal: 8,
                          vertical: 3,
                        ),
                        decoration: BoxDecoration(
                          color: hk.surface3,
                          borderRadius: BorderRadius.circular(5),
                          border: Border.all(color: hk.borderStrong),
                        ),
                        child: Text(
                          k,
                          style: const TextStyle(
                            fontSize: 12,
                            fontFamily: HkTokens.fontMono,
                          ),
                        ),
                      ),
                      const SizedBox(width: HkTokens.s3),
                      Expanded(
                        child: Text(v, style: const TextStyle(fontSize: 13)),
                      ),
                    ],
                  ),
                ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
        ],
      );
    },
  );
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

  /// Grouped like an editor: edit, enhance, deliver; `null` = divider.
  static const _items = <(String, String, IconData)?>[
    ('clips', 'Clips', Icons.video_library_outlined),
    ('captions', 'Captions', Icons.closed_caption_outlined),
    ('broll', 'B-roll', Icons.collections_outlined),
    ('motion', 'Motion', Icons.animation),
    null,
    ('enhance', 'AI enhance', Icons.auto_awesome),
    ('brand', 'Brand', Icons.verified_outlined),
    ('text', 'Write', Icons.text_fields),
    ('thumbs', 'Thumbs', Icons.photo_library_outlined),
    ('agent', 'Agent', Icons.smart_toy_outlined),
    null,
    ('publish', 'Publish', Icons.ios_share),
    ('export', 'Export', Icons.file_download_outlined),
    ('stats', 'Analytics', Icons.insights_outlined),
  ];

  static (String, String, IconData) _find(String k) => _items
      .whereType<(String, String, IconData)>()
      .firstWhere((e) => e.$1 == k, orElse: () => _items.first!);
  static String label(String k) => _find(k).$2;
  static IconData icon(String k) => _find(k).$3;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      width: 76,
      color: hk.surface,
      child: ListView(
        padding: const EdgeInsets.symmetric(vertical: HkTokens.s2),
        children: [
          for (final item in _items)
            if (item == null)
              Padding(
                padding: const EdgeInsets.symmetric(
                  horizontal: 18,
                  vertical: 6,
                ),
                child: Divider(height: 1, color: hk.border),
              )
            else
              _railButton(context, item),
        ],
      ),
    );
  }

  Widget _railButton(BuildContext context, (String, String, IconData) item) {
    final hk = context.hk;
    final (k, label, icon) = item;
    final sel = selected == k;
    return Tooltip(
      message: label,
      waitDuration: const Duration(milliseconds: 600),
      child: InkWell(
        onTap: () => onSelect(k),
        borderRadius: BorderRadius.circular(10),
        child: Container(
          margin: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
          padding: const EdgeInsets.symmetric(vertical: 7),
          decoration: BoxDecoration(
            color: sel ? hk.accent.withValues(alpha: 0.16) : null,
            borderRadius: BorderRadius.circular(10),
          ),
          child: Column(
            children: [
              Icon(icon, size: 20, color: sel ? hk.text : hk.textMuted),
              const SizedBox(height: 3),
              Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
                textAlign: TextAlign.center,
                style: TextStyle(
                  fontSize: 10.5,
                  fontWeight: sel ? FontWeight.w600 : FontWeight.w500,
                  color: sel ? hk.text : hk.textMuted,
                ),
              ),
            ],
          ),
        ),
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
      final o => o,
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
                ctl.reframe(clip.id, aspect: v);
              }
            },
            itemBuilder: (_) => const [
              PopupMenuItem(
                value: '9:16',
                child: Text('9:16  ·  Shorts, Reels, TikTok'),
              ),
              PopupMenuItem(value: '1:1', child: Text('1:1  ·  Square')),
              PopupMenuItem(
                value: '4:5',
                child: Text('4:5  ·  Instagram feed'),
              ),
              PopupMenuItem(value: '16:9', child: Text('16:9  ·  YouTube')),
              PopupMenuDivider(),
              PopupMenuItem(value: 'original', child: Text('Original framing')),
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

/// B-roll placements as `set_broll` JSON (output seconds, as the plan holds them).
List<Map<String, Object?>> brollJson(Iterable<BrollItem> items) => [
  for (final b in items)
    {
      'start': b.start,
      'end': b.end,
      'query': b.query,
      'asset': b.asset,
      'kind': b.isImage ? 'image' : 'video',
    },
];

class _BrollPanel extends ConsumerWidget {
  const _BrollPanel({required this.projectId});
  final String projectId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final s = ref.watch(studioControllerProvider(projectId));
    final ctl = ref.read(studioControllerProvider(projectId).notifier);
    final clip = s.clip;
    final items = s.plan?.broll ?? const <BrollItem>[];
    final busy = s.busy.contains('broll');
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Text(
          'Stock footage over the sentences that need it. Sources: Pexels, Pixabay or your own folders (Studio settings).',
          style: TextStyle(fontSize: 12, color: hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s4),
        if (clip == null)
          const EmptyState(
            icon: Icons.collections_outlined,
            title: 'No clip selected',
            message: 'Pick a clip to add B-roll.',
          )
        else ...[
          GradientButton(
            label: busy
                ? 'Finding footage…'
                : (items.isEmpty ? 'Auto B-roll' : 'Find more B-roll'),
            icon: Icons.auto_awesome,
            busy: busy,
            expand: true,
            onPressed: () => ctl.broll(clip.id),
          ),
          SectionLabel(
            '${items.length} overlay${items.length == 1 ? '' : 's'}',
            trailing: items.isEmpty
                ? null
                : TextButton(
                    onPressed: () => ctl.edit([
                      {
                        'op': 'set_broll',
                        'clip': clip.id,
                        'broll': <Object?>[],
                      },
                    ]),
                    child: const Text('Remove all'),
                  ),
          ),
          for (var i = 0; i < items.length; i++)
            Padding(
              padding: const EdgeInsets.only(bottom: HkTokens.s2),
              child: HoverCard(
                padding: const EdgeInsets.fromLTRB(12, 8, 4, 8),
                child: Row(
                  children: [
                    Icon(
                      items[i].isImage
                          ? Icons.image_outlined
                          : Icons.movie_outlined,
                      size: 18,
                      color: HkTokens.nodeData,
                    ),
                    const SizedBox(width: HkTokens.s3),
                    Expanded(
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Text(
                            items[i].query.isEmpty ? 'B-roll' : items[i].query,
                            maxLines: 1,
                            overflow: TextOverflow.ellipsis,
                            style: const TextStyle(
                              fontSize: 13,
                              fontWeight: FontWeight.w500,
                            ),
                          ),
                          Text(
                            '${formatTime(items[i].start)} – ${formatTime(items[i].end)}',
                            style: TextStyle(fontSize: 11, color: hk.textFaint),
                          ),
                        ],
                      ),
                    ),
                    IconButton(
                      tooltip: 'Remove',
                      icon: const Icon(Icons.delete_outline, size: 18),
                      onPressed: () => ctl.edit([
                        {
                          'op': 'set_broll',
                          'clip': clip.id,
                          'broll': brollJson([
                            for (var k = 0; k < items.length; k++)
                              if (k != i) items[k],
                          ]),
                        },
                      ]),
                    ),
                  ],
                ),
              ),
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
  double _perMin = 6;
  bool _busy = false;

  Future<void> _punch(String clip, Map<String, Object?>? spec) async {
    setState(() => _busy = true);
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    await guarded(
      context,
      () => studioCall('set_punch_in', {
        'id': widget.projectId,
        'clip': clip,
        'punch_in': spec,
      }),
      ok: spec == null ? 'Auto zoom off' : 'Auto zoom applied',
    );
    await ctl.reload();
    if (mounted) setState(() => _busy = false);
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final s = ref.watch(studioControllerProvider(widget.projectId));
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final clip = s.clip;
    final tracking = s.busy.contains('reframe');
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Text(
          'Keep the speaker in frame and punch in on emphasised words.',
          style: TextStyle(fontSize: 12, color: hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s4),
        if (clip == null)
          const EmptyState(
            icon: Icons.animation,
            title: 'No clip selected',
            message: 'Pick a clip to add motion.',
          )
        else ...[
          const SectionLabel('1 · Reframe'),
          OutlinedButton.icon(
            onPressed: tracking ? null : () => ctl.reframe(clip.id),
            icon: tracking
                ? const SizedBox(
                    width: 16,
                    height: 16,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.center_focus_strong, size: 18),
            label: Text(
              tracking
                  ? 'Tracking speaker…'
                  : (clip.reframe == null
                        ? 'Track speaker (9:16)'
                        : 'Re-track speaker'),
            ),
          ),
          if (clip.reframe != null)
            Padding(
              padding: const EdgeInsets.only(top: HkTokens.s2),
              child: Pill('Reframed', icon: Icons.check, color: hk.ok),
            ),
          const SectionLabel('2 · Auto zoom'),
          SliderRow(
            label: 'Punch-in zoom',
            value: _zoom,
            min: 1.05,
            max: 1.4,
            divisions: 35,
            format: (v) => '${v.toStringAsFixed(2)}×',
            onChanged: (v) => setState(() => _zoom = v ?? 1.15),
          ),
          SliderRow(
            label: 'Hold',
            value: _hold,
            min: 0.3,
            max: 2.0,
            divisions: 17,
            format: (v) => '${v.toStringAsFixed(1)} s',
            onChanged: (v) => setState(() => _hold = v ?? 0.8),
          ),
          SliderRow(
            label: 'Max zooms per minute',
            value: _perMin,
            min: 1,
            max: 12,
            divisions: 11,
            onChanged: (v) => setState(() => _perMin = v ?? 6),
          ),
          const SizedBox(height: HkTokens.s2),
          if (clip.reframe == null)
            Padding(
              padding: const EdgeInsets.only(bottom: HkTokens.s2),
              child: Text(
                'Auto zoom needs a reframe first (step 1).',
                style: TextStyle(fontSize: 12, color: hk.warn),
              ),
            ),
          Row(
            children: [
              Expanded(
                child: GradientButton(
                  label: 'Apply auto zoom',
                  icon: Icons.zoom_in,
                  busy: _busy,
                  expand: true,
                  onPressed: clip.reframe == null
                      ? null
                      : () => _punch(clip.id, {
                          'zoom': _zoom,
                          'hold': _hold,
                          'max_per_minute': _perMin.round(),
                        }),
                ),
              ),
              const SizedBox(width: HkTokens.s2),
              OutlinedButton(
                onPressed: _busy ? null : () => _punch(clip.id, null),
                child: const Text('Off'),
              ),
            ],
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
