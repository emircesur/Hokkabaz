import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:file_selector/file_selector.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:media_kit/media_kit.dart';
import 'package:media_kit_video/media_kit_video.dart';

import '../../ffi/ffi.dart';
import '../../src/rust/api/chat.dart' as chat;
import '../../src/rust/api/settings.dart' as settings;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart';
import '../chat/conversation_pane.dart';
import 'studio_api.dart';
import 'concat_editor.dart';
import 'studio_connections.dart';
import 'studio_social.dart';

class StudioScreen extends ConsumerStatefulWidget {
  const StudioScreen({super.key});
  @override
  ConsumerState<StudioScreen> createState() => _StudioScreenState();
}

class _StudioScreenState extends ConsumerState<StudioScreen> {
  final _player = Player();
  late final _video = VideoController(_player);
  final _search = TextEditingController();
  final _prompt = TextEditingController(
    text: 'Make 5 engaging shorts from this video. Remove fillers and add bold captions.',
  );
  List<Json> _projects = [];
  List<Json> _jobs = [];
  Json? _project;
  String? _clipId;
  String? _sessionId;
  String? _busy;
  String? _error;
  String? _notice;
  Uint8List? _frame;
  bool _localPlayback = false;
  bool _polling = false;
  double _time = 0;
  Timer? _timer;
  Timer? _previewTimer;
  Json? _playback;
  bool _fullEditor = false;
  bool _previewLoading = false;
  bool _previewPending = false;
  String? _previewError;
  StreamSubscription<Duration>? _position;

  Json get _edit => object(_project!['edit']);
  List<Json> get _words => _project?['transcript'] == null
      ? []
      : objects(_project!['transcript']['words']);
  List<Json> get _candidates =>
      _project == null ? [] : objects(_project!['candidates']);
  double get _duration =>
      (_project?['source']['info']['duration'] as num?)?.toDouble() ?? 0;
  bool get _available => _project != null && _busy == null;

  @override
  void initState() {
    super.initState();
    unawaited(_run('Loading Studio', _refresh));
    _position = _player.stream.position.listen((position) {
      if (mounted && !_previewLoading) {
        setState(() => _time = position.inMilliseconds / 1000.0);
      }
    });
    _timer = Timer.periodic(
      const Duration(seconds: 2),
      (_) => unawaited(_poll()),
    );
  }

  @override
  void dispose() {
    _timer?.cancel();
    _previewTimer?.cancel();
    unawaited(_position?.cancel());
    unawaited(_player.dispose());
    _search.dispose();
    _prompt.dispose();
    super.dispose();
  }

  Future<void> _run(String label, Future<void> Function() action) async {
    if (_busy != null) return;
    setState(() {
      _busy = label;
      _error = null;
      _notice = null;
    });
    try {
      if (_fullEditor) await setConcatVisibility(false);
      await action();
    } catch (error) {
      if (mounted) setState(() => _error = describeError(error));
    } finally {
      if (mounted) {
        setState(() => _busy = null);
        if (_fullEditor) await setConcatVisibility(true);
      }
    }
  }

  Future<dynamic> _request(String operation, [Json args = const {}]) async {
    final payload = <String, dynamic>{'project_id': _project?['id'], ...args};
    if (['analyze', 'social', 'auto_broll', 'score_clip'].contains(operation) &&
        payload['model'] == null) {
      final config = object(
        object(await studioRequest('connections'))['config'],
      );
      if ((config['model'] as String? ?? '').isEmpty) {
        try {
          payload['model'] = (await settings.getSettings()).defaultModel;
        } catch (_) {
          /* Remote model selection comes from Studio connections. */
        }
      }
    }
    final result = await studioRequest(operation, payload);
    if (result is Map && result['project'] is Map && mounted) {
      setState(() => _project = object(result['project']));
      _schedulePreview();
    }
    return result;
  }

  Future<void> _refresh() async {
    final list = objects(await studioRequest('projects'));
    if (!mounted) return;
    setState(() => _projects = list);
    if (_project == null && list.isNotEmpty) {
      await _select(list.first['id'] as String);
    }
  }

  Future<void> _select(String id) async {
    final result = object(await studioRequest('get', {'project_id': id}));
    if (!mounted) return;
    setState(() {
      _project = object(result['project']);
      _clipId = null;
      _sessionId = null;
      _time = 0;
      _frame = null;
      _playback = null;
      _fullEditor = false;
    });
    await _player.stop();
    final path = _project!['source']['path'] as String;
    final local = await File(path).exists();
    if (mounted) setState(() => _localPlayback = local);
    if (local) {
      await _loadPlayback();
    } else {
      await _seek(0);
    }
  }

  Future<void> _poll() async {
    if (_polling || !mounted || ref.read(backendErrorProvider) != null) return;
    _polling = true;
    try {
      final jobs = objects(await studioRequest('jobs'));
      final list = objects(await studioRequest('projects'));
      if (!mounted) return;
      setState(() {
        _jobs = jobs;
        _projects = list;
      });
      final id = _project?['id'];
      final latest = list.where((p) => p['id'] == id).firstOrNull;
      if (_busy == null &&
          latest != null &&
          latest['revision'] != _project?['revision']) {
        await _request('get');
      }
    } catch (_) {
      /* Explicit actions surface errors; polling recovers on the next tick. */
    } finally {
      _polling = false;
    }
  }

  Future<void> _seek(double time) async {
    if (_project == null) return;
    setState(() => _time = time.clamp(0, _previewDuration));
    if (_localPlayback) {
      await _player.seek(Duration(milliseconds: (_time * 1000).round()));
    } else {
      final data = object(await _request('preview', {'time': _time}));
      if (mounted) {
        setState(
          () =>
              _frame = Uint8List.fromList((data['bytes'] as List).cast<int>()),
        );
      }
    }
  }

  double get _previewDuration =>
      (_playback?['duration'] as num?)?.toDouble() ?? _duration;

  void _schedulePreview() {
    _previewTimer?.cancel();
    _previewTimer = Timer(
      const Duration(milliseconds: 200),
      () => unawaited(_loadPlayback()),
    );
  }

  Future<void> _loadPlayback() async {
    if (!_localPlayback || _project == null || !mounted) return;
    if (_previewLoading) {
      _previewPending = true;
      return;
    }
    _previewLoading = true;
    final id = _project!['id'];
    final clip = _clipId;
    try {
      final data = object(
        await studioRequest('playback', {'project_id': id, 'clip_id': clip}),
      );
      if (!mounted || _project?['id'] != id || clip != _clipId) return;
      if (_playback?['key'] == data['key']) return;
      final resume = _player.state.playing;
      final time = _time.clamp(0.0, (data['duration'] as num).toDouble());
      final native = _player.platform;
      if (native is! NativePlayer) {
        throw StateError('Live editing needs the native video player.');
      }
      await native.setProperty('lavfi-complex', '');
      await _player.open(Media(data['path'] as String), play: false);
      await native.setProperty('lavfi-complex', data['filter'] as String);
      if (time > 0) {
        await _player.seek(Duration(milliseconds: (time * 1000).round()));
      }
      if (resume) await _player.play();
      if (mounted) {
        setState(() {
          _playback = data;
          _previewError = null;
          _time = time;
        });
      }
    } catch (error) {
      if (mounted) {
        await _player.pause();
        setState(() => _previewError = describeError(error));
      }
    } finally {
      _previewLoading = false;
      if (_previewPending) {
        _previewPending = false;
        _schedulePreview();
      }
    }
  }

  Future<void> _seekSource(double sourceTime) async {
    if (_playback == null) {
      await _seek(sourceTime);
      return;
    }
    double offset = 0;
    for (final range in objects(_playback!['ranges'])) {
      final start = (range['start'] as num).toDouble(),
          end = (range['end'] as num).toDouble();
      if (sourceTime < end) {
        await _seek(offset + (sourceTime - start).clamp(0, end - start));
        return;
      }
      offset += end - start;
    }
    await _seek(offset);
  }

  Future<void> _change(Json changes) async {
    await _request('edit', {
      'revision': _project!['revision'],
      'edit': {..._edit, ...changes},
    });
  }

  Future<String?> _text(
    String title,
    String label, {
    String initial = '',
  }) async {
    final controller = TextEditingController(text: initial);
    final result = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(title),
        content: SizedBox(
          width: 480,
          child: TextField(
            controller: controller,
            autofocus: true,
            decoration: InputDecoration(labelText: label),
            onSubmitted: (value) => Navigator.pop(context, value),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text),
            child: const Text('Continue'),
          ),
        ],
      ),
    );
    // Dialog route may still animate while its TextField is being disposed.
    WidgetsBinding.instance.addPostFrameCallback((_) => controller.dispose());
    return result?.trim().isEmpty == true ? null : result?.trim();
  }

  Future<void> _import() async {
    final path = await _text(
      'Import video',
      'File path on this device or the connected instance',
    );
    if (path == null) return;
    final result = object(await studioRequest('import', {'path': path}));
    await _refresh();
    await _select(object(result['project'])['id'] as String);
  }

  Future<void> _pickVideo() async {
    final file = await openFile(
      acceptedTypeGroups: [
        const XTypeGroup(
          label: 'Videos',
          extensions: ['mp4', 'mov', 'mkv', 'webm', 'avi', 'm4v'],
        ),
      ],
    );
    if (file == null) return;
    final result = object(await studioRequest('import', {'path': file.path}));
    await _refresh();
    await _select(object(result['project'])['id'] as String);
  }

  Future<void> _analyze() async {
    final rubric = await _text(
      'Find highlights',
      'Optional focus, audience or custom scoring rubric',
      initial: '5 short clips with strong hooks; prefer complete thoughts.',
    );
    if (rubric == null) return;
    await _request('analyze', {
      'count': 5,
      'min_seconds': 15.0,
      'max_seconds': 60.0,
      'rubric': rubric,
    });
  }

  Future<void> _agent() async {
    if (_sessionId == null) {
      final config = object(
        object(await studioRequest('connections'))['config'],
      );
      final source = _project!['source']['path'] as String;
      final session = await chat.createSession(
        title: 'Studio: ${_project!['name']}',
        model: (config['model'] as String?)?.isNotEmpty == true
            ? config['model'] as String
            : null,
        workspaceRoot: File(source).parent.path,
      );
      if (!mounted) return;
      setState(() => _sessionId = session.id);
      await ref.read(sessionListProvider.notifier).refresh();
    }
    final id = _sessionId!;
    // Attach the existing chat controller before sending so live events are not missed.
    ref.read(chatControllerProvider(id));
    await Future<void>.delayed(const Duration(milliseconds: 100));
    if (!mounted) return;
    await ref
        .read(chatControllerProvider(id).notifier)
        .send(
          'Use Studio project_id=${_project!['id']}. Inspect it with media_probe first. '
          'Keep edits in Studio so the screen reflects them. Never publish without explicit approval. ${_prompt.text}',
          ref.read(chatModeProvider),
        );
  }

  Future<void> _importTranscript() async {
    final file = await openFile(
      acceptedTypeGroups: [
        const XTypeGroup(label: 'Transcript JSON', extensions: ['json']),
      ],
    );
    if (file == null) return;
    await _request('import_transcript', {
      'transcript': jsonDecode(await file.readAsString()),
    });
  }

  @override
  Widget build(BuildContext context) {
    final backendError = ref.watch(backendErrorProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.all(12),
          child: Wrap(
            spacing: 8,
            runSpacing: 8,
            crossAxisAlignment: WrapCrossAlignment.center,
            children: [
              const Icon(Icons.movie_creation_outlined),
              Text('Studio', style: Theme.of(context).textTheme.titleLarge),
              SegmentedButton<bool>(
                segments: const [
                  ButtonSegment(
                    value: false,
                    icon: Icon(Icons.auto_awesome),
                    label: Text('Shorts'),
                  ),
                  ButtonSegment(
                    value: true,
                    icon: Icon(Icons.movie_edit),
                    label: Text('Full editor'),
                  ),
                ],
                selected: {_fullEditor},
                onSelectionChanged: (values) async {
                  await _player.pause();
                  if (mounted) setState(() => _fullEditor = values.first);
                },
              ),
              const SizedBox(width: 12),
              FilledButton.tonalIcon(
                onPressed: _busy == null
                    ? () => _run('Importing video', _pickVideo)
                    : null,
                icon: const Icon(Icons.video_file_outlined),
                label: const Text('Import video'),
              ),
              IconButton(
                tooltip: 'Import a path on the connected instance',
                onPressed: _busy == null
                    ? () => _run('Importing video', _import)
                    : null,
                icon: const Icon(Icons.add_link),
              ),
              OutlinedButton(
                onPressed: _available
                    ? () => _run('Transcribing audio', () async {
                        await _request('transcribe');
                      })
                    : null,
                child: const Text('Transcribe'),
              ),
              OutlinedButton(
                onPressed: _available
                    ? () => _run('Finding highlights', _analyze)
                    : null,
                child: const Text('Find highlights'),
              ),
              FilledButton.icon(
                onPressed: _available
                    ? () => _run('Starting render', () async {
                        await _request('render', {'clip_id': _clipId});
                        await _poll();
                      })
                    : null,
                icon: const Icon(Icons.file_upload_outlined),
                label: const Text('Render'),
              ),
              IconButton(
                tooltip: 'Connections',
                onPressed: _busy == null
                    ? () => _run(
                        'Loading connections',
                        () => showStudioConnections(context),
                      )
                    : null,
                icon: const Icon(Icons.tune),
              ),
              IconButton(
                tooltip: 'Refresh projects',
                onPressed: _busy == null
                    ? () => _run('Refreshing', _refresh)
                    : null,
                icon: const Icon(Icons.refresh),
              ),
            ],
          ),
        ),
        if (_busy != null) ...[
          const LinearProgressIndicator(),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 4),
            child: Text('$_busy…'),
          ),
        ],
        if (backendError != null || _error != null)
          _banner(backendError ?? _error!, error: true),
        if (_notice != null) _banner(_notice!),
        Divider(height: 1, color: context.hk.border),
        Expanded(
          child: LayoutBuilder(
            builder: (context, constraints) {
              if (_fullEditor) {
                return ConcatEditor(
                  key: ValueKey('${_project?['id']}:$_clipId'),
                  projectId: _project?['id'] as String?,
                  clipId: _clipId,
                );
              }
              if (_project == null) return _empty();
              if (constraints.maxWidth >= 1150) {
                return Row(
                  crossAxisAlignment: CrossAxisAlignment.stretch,
                  children: [
                    SizedBox(width: 210, child: _library()),
                    VerticalDivider(width: 1, color: context.hk.border),
                    Expanded(
                      child: Column(
                        children: [
                          Expanded(flex: 4, child: _playerPane()),
                          _timeline(),
                          Expanded(flex: 5, child: _transcript()),
                        ],
                      ),
                    ),
                    VerticalDivider(width: 1, color: context.hk.border),
                    SizedBox(
                      width: 360,
                      child: DefaultTabController(
                        length: 4,
                        child: Column(
                          children: [
                            const TabBar(
                              isScrollable: true,
                              tabs: [
                                Tab(text: 'Clips'),
                                Tab(text: 'Style'),
                                Tab(text: 'Exports'),
                                Tab(text: 'Agent'),
                              ],
                            ),
                            Expanded(
                              child: TabBarView(
                                children: [
                                  _clips(),
                                  _style(),
                                  _exports(),
                                  _agentPane(),
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
              return DefaultTabController(
                length: 6,
                child: Column(
                  children: [
                    const TabBar(
                      isScrollable: true,
                      tabs: [
                        Tab(text: 'Source'),
                        Tab(text: 'Transcript'),
                        Tab(text: 'Clips'),
                        Tab(text: 'Style'),
                        Tab(text: 'Exports'),
                        Tab(text: 'Agent'),
                      ],
                    ),
                    Expanded(
                      child: TabBarView(
                        children: [
                          Column(
                            children: [
                              SizedBox(
                                height: 68,
                                child: _library(compact: true),
                              ),
                              Expanded(child: _playerPane()),
                              _timeline(),
                            ],
                          ),
                          _transcript(),
                          _clips(),
                          _style(),
                          _exports(),
                          _agentPane(),
                        ],
                      ),
                    ),
                  ],
                ),
              );
            },
          ),
        ),
      ],
    );
  }

  Widget _banner(String text, {bool error = false}) => Container(
    padding: const EdgeInsets.all(10),
    color: (error ? context.hk.err : context.hk.info).withValues(alpha: .12),
    child: SelectableText(
      text,
      style: TextStyle(color: error ? context.hk.err : context.hk.text),
    ),
  );

  Widget _empty() => Center(
    child: ConstrainedBox(
      constraints: const BoxConstraints(maxWidth: 520),
      child: Padding(
        padding: const EdgeInsets.all(24),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(
              Icons.auto_awesome_motion_outlined,
              size: 54,
              color: context.hk.accent,
            ),
            const SizedBox(height: 20),
            Text(
              'Long videos. Short stories.',
              style: Theme.of(context).textTheme.headlineSmall,
            ),
            const SizedBox(height: 12),
            const Text(
              'Import a video, transcribe it, then edit by removing words. Find highlights, style captions and export clips — using your own models and APIs.',
              textAlign: TextAlign.center,
            ),
            const SizedBox(height: 20),
            FilledButton(
              onPressed: _busy == null
                  ? () => _run('Importing video', _pickVideo)
                  : null,
              child: const Text('Choose a video'),
            ),
            const SizedBox(height: 12),
            TextButton(
              onPressed: _busy == null
                  ? () => _run('Importing video', _import)
                  : null,
              child: const Text('Use a file path on an instance'),
            ),
          ],
        ),
      ),
    ),
  );

  Widget _library({bool compact = false}) => ListView(
    padding: const EdgeInsets.all(8),
    scrollDirection: compact ? Axis.horizontal : Axis.vertical,
    children: [
      for (final project in _projects)
        SizedBox(
          width: compact ? 220 : null,
          child: ListTile(
            dense: true,
            selected: project['id'] == _project?['id'],
            leading: const Icon(Icons.video_library_outlined),
            title: Text(
              project['name'] as String,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
            ),
            subtitle: Text(timeLabel(project['duration'] as num)),
            onTap: _busy == null
                ? () => _run(
                    'Opening project',
                    () => _select(project['id'] as String),
                  )
                : null,
          ),
        ),
    ],
  );

  Widget _playerPane() => Column(
    children: [
      Padding(
        padding: const EdgeInsets.all(8),
        child: Text(
          '${_project!['name']} · live edit preview',
          maxLines: 1,
          overflow: TextOverflow.ellipsis,
        ),
      ),
      if (_previewError != null)
        Padding(
          padding: const EdgeInsets.all(8),
          child: Text(
            'Preview: $_previewError',
            style: TextStyle(color: Theme.of(context).colorScheme.error),
          ),
        ),
      Expanded(
        child: _localPlayback
            ? Video(controller: _video, controls: NoVideoControls)
            : _frame != null
            ? Image.memory(_frame!, fit: BoxFit.contain, gaplessPlayback: true)
            : const Center(
                child: Text('Remote source · scrub to preview frames'),
              ),
      ),
      Row(
        children: [
          IconButton(
            tooltip: _player.state.playing ? 'Pause' : 'Play edited timeline',
            onPressed: _localPlayback && !_previewLoading
                ? () async {
                    await _player.playOrPause();
                    if (mounted) setState(() {});
                  }
                : null,
            icon: Icon(_player.state.playing ? Icons.pause : Icons.play_arrow),
          ),
          IconButton(
            tooltip: 'Back 5 seconds',
            onPressed: () => _seek((_time - 5).clamp(0, _previewDuration)),
            icon: const Icon(Icons.replay_5),
          ),
          Text(timeLabel(_time)),
          Expanded(
            child: Slider(
              value: _time.clamp(0, _previewDuration),
              max: _previewDuration > 0 ? _previewDuration : 1,
              onChanged: (value) => setState(() => _time = value),
              onChangeEnd: (value) => _run('Seeking', () => _seek(value)),
            ),
          ),
          Text(timeLabel(_previewDuration)),
          const SizedBox(width: 12),
        ],
      ),
    ],
  );

  Widget _timeline() {
    final explicit = (_edit['deleted_words'] as List).cast<int>().toSet();
    final restored = (_edit['restored_words'] as List? ?? [])
        .cast<int>()
        .toSet();
    final deleted = _words
        .asMap()
        .entries
        .where(
          (entry) =>
              explicit.contains(entry.key) ||
              (_edit['remove_fillers'] == true &&
                  entry.value['flags']?['filler'] == true &&
                  !restored.contains(entry.key)),
        )
        .length;
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 8),
      decoration: BoxDecoration(
        border: Border.symmetric(
          horizontal: BorderSide(color: context.hk.border),
        ),
      ),
      child: Row(
        children: [
          Icon(Icons.cut, size: 16, color: context.hk.accent),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              '$deleted words removed · ${_edit['aspect']} · live preview',
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
          IconButton(
            tooltip: 'Undo',
            onPressed: _available && (_project!['undo'] as List).isNotEmpty
                ? () => _run('Undoing edit', () async {
                    await _request('undo');
                  })
                : null,
            icon: const Icon(Icons.undo, size: 18),
          ),
          IconButton(
            tooltip: 'Redo',
            onPressed: _available && (_project!['redo'] as List).isNotEmpty
                ? () => _run('Redoing edit', () async {
                    await _request('redo');
                  })
                : null,
            icon: const Icon(Icons.redo, size: 18),
          ),
        ],
      ),
    );
  }

  Widget _transcript() {
    final words = _words;
    final deleted = (_edit['deleted_words'] as List).cast<int>().toSet();
    final restored = (_edit['restored_words'] as List? ?? [])
        .cast<int>()
        .toSet();
    final query = _search.text.trim().toLowerCase();
    final indices = [
      for (var i = 0; i < words.length; i++)
        if (query.isEmpty ||
            (words[i]['text'] as String).toLowerCase().contains(query))
          i,
    ];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.all(10),
          child: Row(
            children: [
              Expanded(
                child: TextField(
                  controller: _search,
                  decoration: const InputDecoration(
                    isDense: true,
                    prefixIcon: Icon(Icons.search),
                    hintText: 'Search transcript',
                  ),
                  onChanged: (_) => setState(() {}),
                ),
              ),
              IconButton(
                tooltip: 'Import transcript JSON',
                onPressed: _available
                    ? () => _run('Importing transcript', _importTranscript)
                    : null,
                icon: const Icon(Icons.upload_file),
              ),
            ],
          ),
        ),
        Wrap(
          spacing: 6,
          children: [
            FilterChip(
              label: const Text('Remove fillers'),
              selected: _edit['remove_fillers'] == true,
              onSelected: _available
                  ? (value) => _run(
                      'Editing',
                      () => _change({'remove_fillers': value}),
                    )
                  : null,
            ),
            FilterChip(
              label: const Text('Remove silences'),
              selected: _edit['remove_silences'] == true,
              onSelected: _available
                  ? (value) => _run(
                      'Editing',
                      () => _change({'remove_silences': value}),
                    )
                  : null,
            ),
          ],
        ),
        Padding(
          padding: const EdgeInsets.all(8),
          child: Text(
            'Click a word to remove or restore it. Long-press to seek.',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
        Expanded(
          child: words.isEmpty
              ? const Center(
                  child: Text(
                    'Transcribe audio or import word-level transcript JSON.',
                  ),
                )
              : ListView.builder(
                  padding: const EdgeInsets.all(12),
                  itemCount: (indices.length / 60).ceil(),
                  itemBuilder: (context, page) {
                    final chunk = indices.skip(page * 60).take(60);
                    return Padding(
                      padding: const EdgeInsets.only(bottom: 8),
                      child: Wrap(
                        spacing: 3,
                        runSpacing: 5,
                        children: [
                          for (final i in chunk)
                            Builder(
                              builder: (context) {
                                final word = words[i];
                                final filler =
                                    (word['flags'] as Map?)?['filler'] == true;
                                final removed =
                                    deleted.contains(i) ||
                                    (_edit['remove_fillers'] == true &&
                                        filler &&
                                        !restored.contains(i));
                                return Tooltip(
                                  message:
                                      '${timeLabel(word['start'] as num)}${filler ? ' · filler' : ''}',
                                  child: InkWell(
                                    borderRadius: BorderRadius.circular(4),
                                    onLongPress: () => _run(
                                      'Seeking',
                                      () => _seekSource(
                                        (word['start'] as num).toDouble(),
                                      ),
                                    ),
                                    onTap: _available
                                        ? () => _run('Editing word', () async {
                                            final next = {...deleted};
                                            final retained = {...restored};
                                            if (removed) {
                                              next.remove(i);
                                              retained.add(i);
                                            } else {
                                              next.add(i);
                                              retained.remove(i);
                                            }
                                            await _change({
                                              'deleted_words': next.toList()
                                                ..sort(),
                                              'restored_words':
                                                  retained.toList()..sort(),
                                            });
                                          })
                                        : null,
                                    child: Container(
                                      padding: const EdgeInsets.symmetric(
                                        horizontal: 5,
                                        vertical: 4,
                                      ),
                                      color: removed
                                          ? context.hk.err.withValues(alpha: .1)
                                          : filler
                                          ? context.hk.warn.withValues(
                                              alpha: .12,
                                            )
                                          : null,
                                      child: Text(
                                        word['text'] as String,
                                        style: TextStyle(
                                          color: removed
                                              ? context.hk.textMuted
                                              : context.hk.text,
                                          decoration: removed
                                              ? TextDecoration.lineThrough
                                              : null,
                                        ),
                                      ),
                                    ),
                                  ),
                                );
                              },
                            ),
                        ],
                      ),
                    );
                  },
                ),
        ),
      ],
    );
  }

  Widget _clips() => ListView(
    padding: const EdgeInsets.all(12),
    children: [
      const Text(
        'Potential reach is an AI estimate. Scores are not a guarantee of views.',
      ),
      const SizedBox(height: 12),
      ListTile(
        selected: _clipId == null,
        leading: const Icon(Icons.movie_outlined),
        title: const Text('Full edited video'),
        onTap: () {
          setState(() {
            _clipId = null;
            _time = 0;
          });
          _schedulePreview();
        },
      ),
      if (_candidates.isEmpty)
        const Padding(
          padding: EdgeInsets.all(20),
          child: Text(
            'Transcribe, then find highlights to rank clip candidates.',
          ),
        ),
      for (final candidate in _candidates)
        Card(
          child: Column(
            children: [
              ListTile(
                selected: _clipId == candidate['id'],
                leading: CircleAvatar(child: Text('${candidate['score']}')),
                title: Text(candidate['title'] as String),
                subtitle: Text(
                  '${timeLabel(candidate['span']['start'] as num)}–${timeLabel(candidate['span']['end'] as num)}\n${candidate['hook']}',
                ),
                onTap: () {
                  setState(() {
                    _clipId = candidate['id'] as String;
                    _time = 0;
                  });
                  _schedulePreview();
                  unawaited(_run('Seeking', () => _seek(0)));
                },
              ),
              ExpansionTile(
                title: const Text('Why this clip'),
                children: [
                  Padding(
                    padding: const EdgeInsets.all(12),
                    child: Text(candidate['reason'] as String),
                  ),
                  TextButton(
                    onPressed: _available
                        ? () => _run('Scoring clip', () async {
                            final rubric = await _text(
                              'Score this clip',
                              'Audience, platform or custom rubric',
                              initial: 'Hook, emotion, clarity and pacing',
                            );
                            if (rubric != null) {
                              await _request('score_clip', {
                                'clip_id': candidate['id'],
                                'rubric': rubric,
                              });
                            }
                          })
                        : null,
                    child: const Text('Re-score with a custom rubric'),
                  ),
                  for (final criterion in objects(candidate['criteria']))
                    ListTile(
                      dense: true,
                      title: Text(
                        '${criterion['name']} · ${criterion['score']}/100',
                      ),
                      subtitle: Text(criterion['reason'] as String),
                    ),
                ],
              ),
            ],
          ),
        ),
    ],
  );

  Widget _section(String title) => Padding(
    padding: const EdgeInsets.only(top: 20, bottom: 10),
    child: Text(title, style: Theme.of(context).textTheme.titleMedium),
  );

  Widget _style() {
    final captions = object(_edit['captions']);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        _section('Format & framing'),
        DropdownButtonFormField<String>(
          key: ValueKey(_edit['aspect']),
          initialValue: _edit['aspect'] as String,
          items: const [
            DropdownMenuItem(
              value: 'vertical',
              child: Text('9:16 · Shorts / Reels'),
            ),
            DropdownMenuItem(value: 'square', child: Text('1:1 · Square')),
            DropdownMenuItem(
              value: 'landscape',
              child: Text('16:9 · Landscape'),
            ),
          ],
          onChanged: _available
              ? (value) =>
                    _run('Changing format', () => _change({'aspect': value}))
              : null,
        ),
        const SizedBox(height: 10),
        OutlinedButton.icon(
          onPressed: _available
              ? () => _run('Planning reframing', () async {
                  await _request('reframe');
                })
              : null,
          icon: const Icon(Icons.center_focus_strong),
          label: const Text('Track subject / center crop'),
        ),
        const Text(
          'Subject tracking uses your detector connection. Without one, the frame stays centered.',
        ),
        _section('Caption templates'),
        Wrap(
          spacing: 6,
          runSpacing: 6,
          children: [
            for (final template in [
              'karaoke',
              'bold_pop',
              'podcast',
              'minimal',
              'neon',
              'cinematic',
              'news',
              'lecture',
              'gaming',
              'clean_white',
              'story',
              'kinetic',
            ])
              ChoiceChip(
                label: Text(template.replaceAll('_', ' ')),
                selected: _edit['template'] == template,
                onSelected: _available
                    ? (_) => _run('Applying template', () async {
                        await _request('template', {'template': template});
                      })
                    : null,
              ),
          ],
        ),
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('Burn in captions'),
          value: captions['enabled'] == true,
          onChanged: _available
              ? (value) => _run(
                  'Editing captions',
                  () => _change({
                    'captions': {...captions, 'enabled': value},
                  }),
                )
              : null,
        ),
        Text('Font size · ${captions['font_size']}'),
        Slider(
          value: (captions['font_size'] as num).toDouble().clamp(24, 120),
          min: 24,
          max: 120,
          divisions: 24,
          onChanged: _available
              ? (value) => _run(
                  'Editing captions',
                  () => _change({
                    'captions': {...captions, 'font_size': value.round()},
                  }),
                )
              : null,
        ),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Editing caption color', () async {
                  final color = await _text(
                    'Highlight color',
                    '#RRGGBB',
                    initial: captions['highlight'] as String,
                  );
                  if (color != null) {
                    await _change({
                      'captions': {...captions, 'highlight': color},
                    });
                  }
                })
              : null,
          child: Text('Highlight · ${captions['highlight']}'),
        ),
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('Uppercase'),
          value: captions['uppercase'] == true,
          onChanged: _available
              ? (value) => _run(
                  'Editing captions',
                  () => _change({
                    'captions': {...captions, 'uppercase': value},
                  }),
                )
              : null,
        ),
        _section('Caption details'),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Editing font', () async {
                  final font = await _text(
                    'Caption font',
                    'Installed font family',
                    initial: captions['font'] as String,
                  );
                  if (font != null) {
                    await _change({
                      'captions': {...captions, 'font': font},
                    });
                  }
                })
              : null,
          child: Text('Font · ${captions['font']}'),
        ),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Editing text color', () async {
                  final color = await _text(
                    'Text color',
                    '#RRGGBB',
                    initial: captions['color'] as String,
                  );
                  if (color != null) {
                    await _change({
                      'captions': {...captions, 'color': color},
                    });
                  }
                })
              : null,
          child: Text('Text · ${captions['color']}'),
        ),
        Text('Outline · ${captions['outline']}'),
        Slider(
          value: (captions['outline'] as num).toDouble().clamp(0, 10),
          min: 0,
          max: 10,
          divisions: 10,
          onChanged: _available
              ? (value) => _run(
                  'Editing outline',
                  () => _change({
                    'captions': {...captions, 'outline': value.round()},
                  }),
                )
              : null,
        ),
        DropdownButtonFormField<int>(
          key: ValueKey('words-${captions['words_per_line']}'),
          initialValue: captions['words_per_line'] as int,
          decoration: const InputDecoration(labelText: 'Words per line'),
          items: [
            for (final words in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])
              DropdownMenuItem(value: words, child: Text('$words')),
          ],
          onChanged: _available
              ? (value) => _run(
                  'Editing line length',
                  () => _change({
                    'captions': {...captions, 'words_per_line': value},
                  }),
                )
              : null,
        ),
        SwitchListTile(
          contentPadding: EdgeInsets.zero,
          title: const Text('Animate caption entrance'),
          value: captions['animated'] == true,
          onChanged: _available
              ? (value) => _run(
                  'Editing animation',
                  () => _change({
                    'captions': {...captions, 'animated': value},
                  }),
                )
              : null,
        ),
        _section('B-roll & music'),
        OutlinedButton(
          onPressed: _available ? () => _run('Adding B-roll', _addBroll) : null,
          child: const Text('Add local B-roll'),
        ),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Finding B-roll', _searchBroll)
              : null,
          child: const Text('Search stock videos'),
        ),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Planning B-roll', () async {
                  await _request('auto_broll', {'provider': 'pexels'});
                })
              : null,
          child: const Text('Auto B-roll · Pexels'),
        ),
        for (final overlay in objects(_edit['broll']))
          ListTile(
            dense: true,
            title: Text(File(overlay['path'] as String).uri.pathSegments.last),
            subtitle: Text(
              '${timeLabel(overlay['span']['start'] as num)}–${timeLabel(overlay['span']['end'] as num)}',
            ),
            trailing: IconButton(
              icon: const Icon(Icons.close),
              onPressed: _available
                  ? () => _run(
                      'Removing B-roll',
                      () => _change({
                        'broll': objects(_edit['broll'])
                            .where((b) => b['path'] != overlay['path'])
                            .toList(),
                      }),
                    )
                  : null,
            ),
          ),
        OutlinedButton(
          onPressed: _available
              ? () => _run('Choosing music', () async {
                  final path = await _text(
                    'Background music',
                    'Audio file path on the active instance',
                  );
                  if (path != null) await _change({'music': path});
                })
              : null,
          child: const Text('Add music · automatic voice ducking'),
        ),
        if (_edit['music'] != null)
          TextButton(
            onPressed: _available
                ? () => _run('Removing music', () => _change({'music': null}))
                : null,
            child: const Text('Remove music'),
          ),
        _section('Fine editing'),
        OutlinedButton.icon(
          onPressed: _available
              ? () async {
                  await _player.pause();
                  if (mounted) setState(() => _fullEditor = true);
                }
              : null,
          icon: const Icon(Icons.movie_edit),
          label: const Text('Edit here in Concat'),
        ),
      ],
    );
  }

  Future<void> _addBroll({Json? asset}) async {
    final path = asset == null
        ? await _text(
            'B-roll file',
            'Video or image path on the active instance',
          )
        : null;
    if (asset == null && path == null) return;
    final startText = await _text(
      'B-roll placement',
      'Start on edited timeline, seconds',
      initial: '0',
    );
    if (startText == null) return;
    final start = double.tryParse(startText);
    if (start == null) throw const FormatException('Enter a time in seconds');
    await _request('add_broll', {
      'path': path,
      if (asset != null) 'url': asset['url'],
      'attribution': asset?['attribution'],
      'span': {'start': start, 'end': start + 3},
    });
  }

  Future<void> _searchBroll() async {
    final query = await _text(
      'Find stock footage',
      'Search Pexels (requires your API key)',
    );
    if (query == null) return;
    final result = object(
      await studioRequest('search_broll', {
        'provider': 'pexels',
        'query': query,
      }),
    );
    final assets = objects(result['assets']);
    if (!mounted) return;
    final asset = await showDialog<Json>(
      context: context,
      builder: (context) => SimpleDialog(
        title: const Text('Stock videos'),
        children: [
          if (assets.isEmpty)
            const Padding(
              padding: EdgeInsets.all(20),
              child: Text('No matching videos.'),
            ),
          for (final asset in assets)
            SimpleDialogOption(
              onPressed: () => Navigator.pop(context, asset),
              child: Text(
                '${asset['provider']} #${asset['id']}\n${asset['attribution']}',
              ),
            ),
        ],
      ),
    );
    if (asset != null) await _addBroll(asset: asset);
  }

  Widget _exports() => ListView(
    padding: const EdgeInsets.all(16),
    children: [
      _section('Render queue'),
      for (final job in _jobs.where((j) => j['project_id'] == _project!['id']))
        Card(
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  '${job['state']} · ${((job['progress'] as num) * 100).round()}%',
                ),
                const SizedBox(height: 6),
                LinearProgressIndicator(
                  value: (job['progress'] as num).toDouble(),
                ),
                if (job['error'] != null)
                  Text(
                    job['error'] as String,
                    style: TextStyle(color: context.hk.err),
                  ),
                if (job['state'] == 'running')
                  TextButton(
                    onPressed: () => _run('Cancelling render', () async {
                      await studioRequest('cancel_render', {
                        'job_id': job['id'],
                      });
                    }),
                    child: const Text('Cancel'),
                  ),
                if (job['state'] == 'completed') ...[
                  SelectableText(
                    job['output'] as String,
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                  TextButton(
                    onPressed: () => Clipboard.setData(
                      ClipboardData(text: job['output'] as String),
                    ),
                    child: const Text('Copy output path'),
                  ),
                  OutlinedButton(
                    onPressed: _available
                        ? () => _run('Preparing upload', () => _publish(job))
                        : null,
                    child: const Text('Upload to YouTube…'),
                  ),
                ],
              ],
            ),
          ),
        ),
      if (_jobs.where((j) => j['project_id'] == _project!['id']).isEmpty)
        const Text('Render the full edit or select a clip first.'),
      _section('Social metadata'),
      Wrap(
        spacing: 6,
        children: [
          for (final platform in ['youtube', 'tiktok', 'instagram', 'linkedin'])
            ActionChip(
              label: Text(platform),
              onPressed: _available
                  ? () => _run('Writing social metadata', () async {
                      await _request('social', {
                        'platform': platform,
                        'clip_id': _clipId,
                      });
                    })
                  : null,
            ),
        ],
      ),
      for (final social in objects(
        _project!['social'],
      ).where((s) => s['clip_id'] == _clipId))
        Card(
          child: Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  social['platform'] as String,
                  style: Theme.of(context).textTheme.labelLarge,
                ),
                TextButton(
                  onPressed: _available
                      ? () => _run('Editing metadata', () async {
                          final metadata = await editSocialMetadata(
                            context,
                            social,
                          );
                          if (metadata != null) {
                            await _request('save_social', {
                              'metadata': metadata,
                              'clip_id': social['clip_id'],
                            });
                          }
                        })
                      : null,
                  child: const Text('Edit before publishing'),
                ),
                SelectableText(
                  '${social['title']}\n\n${social['description']}\n\n${(social['hashtags'] as List).join(' ')}',
                ),
                TextButton(
                  onPressed: () => Clipboard.setData(
                    ClipboardData(
                      text:
                          '${social['title']}\n\n${social['description']}\n\n${(social['hashtags'] as List).join(' ')}',
                    ),
                  ),
                  child: const Text('Copy metadata'),
                ),
              ],
            ),
          ),
        ),
      _section('Export package'),
      const Text(
        'SRT, VTT, styled ASS, social metadata, attribution and an editable Concat plan. Includes a completed video when available.',
      ),
      const SizedBox(height: 10),
      FilledButton.tonal(
        onPressed: _available
            ? () => _run('Exporting package', () async {
                final result = object(
                  await _request('export_package', {'clip_id': _clipId}),
                );
                if (mounted) {
                  setState(
                    () => _notice = 'Exported to ${result['directory']}',
                  );
                }
              })
            : null,
        child: const Text('Export package'),
      ),
    ],
  );

  Future<void> _publish(Json job) async {
    final metadata = objects(_project!['social'])
        .where(
          (s) => s['platform'] == 'youtube' && s['clip_id'] == job['clip_id'],
        )
        .firstOrNull;
    if (metadata == null) {
      throw const FormatException(
        'Select this rendered clip and generate YouTube metadata first.',
      );
    }
    final approved = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Upload to YouTube?'),
        content: Text(
          '${metadata['title']}\n\n${metadata['description']}\n\nVisibility: private. Review it in YouTube Studio before publishing.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('Upload privately'),
          ),
        ],
      ),
    );
    if (approved != true) return;
    final result = object(
      await _request('publish', {
        'job_id': job['id'],
        'privacy': 'private',
        'confirmed': true,
      }),
    );
    if (mounted) {
      setState(() => _notice = 'Uploaded to YouTube · video ${result['id']}');
    }
  }

  Widget _agentPane() => _sessionId != null
      ? ConversationPane(key: ValueKey(_sessionId), sessionId: _sessionId!)
      : Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                'Studio agent',
                style: Theme.of(context).textTheme.titleLarge,
              ),
              const SizedBox(height: 12),
              const Text(
                'Ask your selected model to transcribe, edit, find highlights and render. Tool activity and approvals appear here.',
              ),
              const SizedBox(height: 16),
              TextField(
                controller: _prompt,
                minLines: 3,
                maxLines: 7,
                decoration: const InputDecoration(
                  labelText: 'What should we make?',
                ),
              ),
              const SizedBox(height: 12),
              FilledButton.icon(
                onPressed: _available
                    ? () => _run('Starting Studio agent', _agent)
                    : null,
                icon: const Icon(Icons.auto_awesome),
                label: const Text('Start editing'),
              ),
            ],
          ),
        );
}
