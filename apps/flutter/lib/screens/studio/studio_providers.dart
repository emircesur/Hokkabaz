import 'dart:async';
import 'dart:convert';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart' show rustInitErrorProvider;
import '../../src/rust/api/studio.dart' as api;
import '../chat/chat_providers.dart' show describeError;
import 'studio_models.dart';
import 'studio_transfer.dart';

/// Project list + import / delete.
class StudioList extends AsyncNotifier<List<ProjectSummary>> {
  @override
  Future<List<ProjectSummary>> build() async {
    if (ref.watch(rustInitErrorProvider) != null) return const [];
    return _load();
  }

  Future<List<ProjectSummary>> _load() async {
    final raw = jsonDecode(await api.studioProjects()) as List;
    return raw.map((e) => ProjectSummary.fromJson((e as Map).cast())).toList();
  }

  Future<void> refresh() async => state = AsyncData(await _load());

  /// Returns the new project id, or throws.
  Future<String> import(String path, {String? name, void Function(double)? onProgress}) async {
    // On a connected instance the video has to get there first.
    final where = await studioUploadFor(path, onProgress: onProgress);
    final p = StudioProjectData.parse(await api.studioImport(path: where, name: name ?? path.split(RegExp(r'[\\/]')).last));
    await refresh();
    return p.id;
  }

  Future<void> remove(String id) async {
    await api.studioDelete(id: id);
    await refresh();
  }
}

final studioListProvider = AsyncNotifierProvider<StudioList, List<ProjectSummary>>(StudioList.new);

class SelectedStudioProject extends Notifier<String?> {
  @override
  String? build() => null;
  void select(String? id) => state = id;
}

final selectedStudioProjectProvider = NotifierProvider<SelectedStudioProject, String?>(SelectedStudioProject.new);

class StudioState {
  const StudioState({
    this.project,
    this.plan,
    this.selectedClip,
    this.canUndo = false,
    this.canRedo = false,
    this.busy = const {},
    this.notice,
    this.renderProgress = const {},
    this.rendered = const {},
    this.loading = true,
    this.lastEditBy,
  });

  final StudioProjectData? project;
  final PreviewPlan? plan;
  final String? selectedClip;
  final bool canUndo;
  final bool canRedo;

  /// Running long jobs by name (`transcribe`, `analyze`, `reframe`, `broll`, `social`, `render`).
  final Set<String> busy;
  final String? notice;
  final Map<String, double> renderProgress;
  final Map<String, String> rendered;
  final bool loading;

  /// Who made the most recent edit (`ui` / `agent`), to flash "agent edited" in the UI.
  final String? lastEditBy;

  StudioClip? get clip => project?.clip(selectedClip);

  StudioState copyWith({
    StudioProjectData? project,
    PreviewPlan? plan,
    bool clearPlan = false,
    String? selectedClip,
    bool clearClip = false,
    bool? canUndo,
    bool? canRedo,
    Set<String>? busy,
    String? notice,
    bool clearNotice = false,
    Map<String, double>? renderProgress,
    Map<String, String>? rendered,
    bool? loading,
    String? lastEditBy,
  }) => StudioState(
    project: project ?? this.project,
    plan: clearPlan ? null : (plan ?? this.plan),
    selectedClip: clearClip ? null : (selectedClip ?? this.selectedClip),
    canUndo: canUndo ?? this.canUndo,
    canRedo: canRedo ?? this.canRedo,
    busy: busy ?? this.busy,
    notice: clearNotice ? null : (notice ?? this.notice),
    renderProgress: renderProgress ?? this.renderProgress,
    rendered: rendered ?? this.rendered,
    loading: loading ?? this.loading,
    lastEditBy: lastEditBy ?? this.lastEditBy,
  );
}

/// One open project: state + every action the editor and panels need. Edits made
/// by the agent arrive through the event stream and update the same state.
class StudioController extends Notifier<StudioState> {
  StudioController(this.projectId);

  final String projectId;
  StreamSubscription<String>? _sub;
  Timer? _debounce;
  bool _disposed = false;

  @override
  StudioState build() {
    ref.onDispose(() {
      _disposed = true;
      _debounce?.cancel();
      _sub?.cancel();
    });
    _sub = api.studioEvents(id: projectId).listen(_onEvent, onError: (Object e) => _notice(describeError(e)));
    Future.microtask(reload);
    return const StudioState();
  }

  void _notice(String m) {
    if (!_disposed) state = state.copyWith(notice: m);
  }

  void clearNotice() => state = state.copyWith(clearNotice: true);

  void _onEvent(String raw) {
    if (_disposed) return;
    final StudioEvent ev;
    try {
      ev = StudioEvent.parse(raw);
    } catch (_) {
      return;
    }
    switch (ev.kind) {
      case 'changed':
        state = state.copyWith(lastEditBy: ev.by);
        _scheduleReload();
      case 'clips_changed' || 'transcribed' || 'resync':
        _scheduleReload();
      case 'render_progress':
        state = state.copyWith(renderProgress: {...state.renderProgress, ev.clip ?? '': ev.fraction ?? 0});
      case 'render_done':
        final progress = {...state.renderProgress}..remove(ev.clip);
        state = state.copyWith(
          renderProgress: progress,
          rendered: {...state.rendered, ev.clip ?? '': ev.path ?? ''},
          busy: {...state.busy}..remove('render'),
          notice: 'Render finished',
        );
      case 'render_failed':
        final progress = {...state.renderProgress}..remove(ev.clip);
        state = state.copyWith(renderProgress: progress, busy: {...state.busy}..remove('render'), notice: 'Render failed: ${ev.error}');
    }
  }

  void _scheduleReload() {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 80), reload);
  }

  Future<void> reload() async {
    try {
      final project = StudioProjectData.parse(await api.studioProject(id: projectId));
      var selected = state.selectedClip;
      if (project.clip(selected) == null) selected = project.clips.firstOrNull?.id;
      final h = jsonDecode(await api.studioHistory(id: projectId)) as Map;
      PreviewPlan? plan;
      if (selected != null) {
        plan = PreviewPlan.parse(await api.studioPreview(id: projectId, clip: selected));
        if (studioRemote && plan.broll.isNotEmpty) {
          // Stock images live on the instance: hand the player URLs.
          plan = plan.withBroll([
            for (final b in plan.broll) b.withAsset(b.asset.isEmpty ? '' : await studioMediaFor(b.asset)),
          ]);
        }
      }
      if (_disposed) return;
      final rendered = {...state.rendered};
      for (final c in project.clips) {
        if (!rendered.containsKey(c.id)) {
          final p = await api.studioRenderPath(id: projectId, clip: c.id);
          if (p != null) rendered[c.id] = p;
        }
      }
      if (_disposed) return;
      state = StudioState(
        project: project,
        plan: plan,
        selectedClip: selected,
        canUndo: h['can_undo'] == true,
        canRedo: h['can_redo'] == true,
        busy: state.busy,
        notice: state.notice,
        renderProgress: state.renderProgress,
        rendered: rendered,
        loading: false,
        lastEditBy: state.lastEditBy,
      );
    } catch (e) {
      if (!_disposed) state = state.copyWith(loading: false, notice: describeError(e));
    }
  }

  Future<void> selectClip(String? id) async {
    state = state.copyWith(selectedClip: id, clearClip: id == null, clearPlan: true);
    await reload();
  }

  /// Apply edit operations (EditOp maps). The event stream triggers the reload; do it now too so
  /// the UI that made the edit never waits on the broadcast.
  Future<bool> edit(List<Map<String, dynamic>> ops) async {
    try {
      await api.studioEdit(id: projectId, opsJson: jsonEncode(ops), by: 'ui');
      await reload();
      return true;
    } catch (e) {
      _notice(describeError(e));
      return false;
    }
  }

  Future<void> undo() async {
    try {
      await api.studioUndo(id: projectId);
      await reload();
    } catch (e) {
      _notice(describeError(e));
    }
  }

  Future<void> redo() async {
    try {
      await api.studioRedo(id: projectId);
      await reload();
    } catch (e) {
      _notice(describeError(e));
    }
  }

  Future<T?> _job<T>(String name, Future<T> Function() fn, {String? done}) async {
    if (state.busy.contains(name)) return null;
    state = state.copyWith(busy: {...state.busy, name}, clearNotice: true);
    try {
      final out = await fn();
      if (!_disposed) state = state.copyWith(busy: {...state.busy}..remove(name), notice: done);
      await reload();
      return out;
    } catch (e) {
      if (!_disposed) state = state.copyWith(busy: {...state.busy}..remove(name), notice: describeError(e));
      return null;
    }
  }

  Future<void> transcribe() => _job('transcribe', () => api.studioTranscribe(id: projectId), done: 'Transcript ready');

  Future<List<String>?> analyze({int count = 8, double minSecs = 15, double maxSecs = 75, String? brief, String? template}) => _job(
    'analyze',
    () async {
      final raw = await api.studioAnalyze(id: projectId, count: count, minSecs: minSecs, maxSecs: maxSecs, brief: brief, template: template);
      return (jsonDecode(raw) as List).cast<String>();
    },
    done: 'Highlights added',
  );

  Future<void> reframe(String clip, {bool square = false}) => _job('reframe', () => api.studioReframe(id: projectId, clip: clip, square: square), done: 'Subject tracked');

  Future<void> broll(String clip, {int max = 4}) => _job('broll', () => api.studioBroll(id: projectId, clip: clip, max: max), done: 'B-roll added');

  Future<void> social(String clip, {List<String> platforms = const [], String? tone, String? language}) => _job(
    'social',
    () => api.studioSocial(id: projectId, clip: clip, platformsJson: jsonEncode(platforms), tone: tone, language: language),
    done: 'Post copy ready',
  );

  Future<void> render(String clip) async {
    if (state.busy.contains('render')) return;
    state = state.copyWith(busy: {...state.busy, 'render'}, renderProgress: {...state.renderProgress, clip: 0}, clearNotice: true);
    try {
      await api.studioRender(id: projectId, clip: clip);
    } catch (e) {
      state = state.copyWith(busy: {...state.busy}..remove('render'), notice: describeError(e));
    }
  }

  /// Build the clip's Concat project from the current edit and open the Concat window on it.
  Future<void> concatEdit(String clip, {bool open = true, bool rebuild = false}) => _job(
    'concat',
    () => api.studioConcatEdit(id: projectId, clip: clip, open: open, rebuild: rebuild),
    done: open ? 'Concat is open. Edit there, save, close the window, then press "Render with Concat".' : 'Concat project rebuilt from the Studio edit',
  );

  /// Export the clip's Concat project with Concat's engine; progress arrives like a normal render.
  Future<void> concatRender(String clip) async {
    if (state.busy.contains('render')) return;
    state = state.copyWith(busy: {...state.busy, 'render'}, renderProgress: {...state.renderProgress, clip: 0}, clearNotice: true);
    try {
      await api.studioConcatEdit(id: projectId, clip: clip, open: false, rebuild: false);
      await api.studioConcatRender(id: projectId, clip: clip);
    } catch (e) {
      state = state.copyWith(busy: {...state.busy}..remove('render'), renderProgress: {...state.renderProgress}..remove(clip), notice: describeError(e));
    }
  }

  /// Upload to YouTube; returns the video URL.
  Future<String?> publishYoutube(String clip, {String privacy = 'private'}) => _job(
    'publish',
    () => api.studioPublishYoutube(id: projectId, clip: clip, privacy: privacy),
  ).then((url) {
    if (url != null) _notice('Uploaded ($privacy): $url');
    return url;
  });

  /// Remote Studio: fetch the rendered clip and let the user choose where to keep it.
  Future<String?> saveRendered(String clip) async {
    final server = state.rendered[clip];
    if (server == null) {
      _notice('Render the clip first');
      return null;
    }
    try {
      final out = await saveRemoteClip(server, 'clip-$clip.mp4');
      if (out != null) state = state.copyWith(notice: 'Saved $out');
      return out;
    } catch (e) {
      _notice(describeError(e));
      return null;
    }
  }

  Future<String?> export(String clip, String dir) async {
    try {
      final out = await api.studioExport(id: projectId, clip: clip, dir: dir);
      state = state.copyWith(notice: 'Exported to $out');
      return out;
    } catch (e) {
      _notice(describeError(e));
      return null;
    }
  }
}

final studioControllerProvider = NotifierProvider.autoDispose.family<StudioController, StudioState, String>(StudioController.new);

/// Templates, caption presets and platforms.
final studioCatalogProvider = FutureProvider<Map<String, dynamic>>((ref) async {
  if (ref.watch(rustInitErrorProvider) != null) return const {};
  return (jsonDecode(await api.studioTemplates()) as Map).cast<String, dynamic>();
});

/// `{ffmpeg: bool, ...}`; the screen warns when ffmpeg is missing.
final studioToolsProvider = FutureProvider<Map<String, dynamic>>((ref) async {
  if (ref.watch(rustInitErrorProvider) != null) return const {};
  return (jsonDecode(await api.studioCheckTools()) as Map).cast<String, dynamic>();
});
