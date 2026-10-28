import 'dart:async';
import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';
import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import 'tools_panel.dart';
import 'whisper_models.dart';

Future<void> showStudioSettings(BuildContext context) => showDialog<void>(
  context: context,
  builder: (_) => const _StudioSettingsDialog(),
);

class _StudioSettingsDialog extends ConsumerStatefulWidget {
  const _StudioSettingsDialog();

  @override
  ConsumerState<_StudioSettingsDialog> createState() => _State();
}

class _State extends ConsumerState<_StudioSettingsDialog> {
  final _sttUrl = TextEditingController();
  final _sttModel = TextEditingController();
  final _sttLang = TextEditingController();
  final _whisperPath = TextEditingController();
  final _sttKey = TextEditingController();
  final _pexels = TextEditingController();
  final _pixabay = TextEditingController();
  final _brollDir = TextEditingController();
  final _ytId = TextEditingController();
  final _ytSecret = TextEditingController();
  final _ytRefresh = TextEditingController();
  final _igUser = TextEditingController();
  final _liAuthor = TextEditingController();
  final Map<String, TextEditingController> _tokens = {
    for (final p in ['tiktok', 'instagram_reels', 'x', 'linkedin'])
      p: TextEditingController(),
  };
  Map<String, dynamic> _hasToken = {};
  final _gpuDevice = TextEditingController(text: '0');
  bool _whisperGpu = true;
  bool _hasYtSecret = false;
  bool _hasYtRefresh = false;
  String? _llm;
  String? _whisperModel;
  String _sttProvider = 'openai_compat';
  bool _hasStt = false;
  bool _hasPexels = false;
  bool _hasPixabay = false;
  bool _loaded = false;
  bool _saving = false;
  String? _error;
  String? _faceStatus;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    for (final c in [
      _sttUrl,
      _sttModel,
      _sttLang,
      _whisperPath,
      _sttKey,
      _pexels,
      _pixabay,
      _brollDir,
      _ytId,
      _ytSecret,
      _ytRefresh,
      _gpuDevice,
      _faceModelPath,
    ]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final j = jsonDecode(await api.studioGetSettings()) as Map;
      final s = (j['settings'] as Map).cast<String, dynamic>();
      setState(() {
        _llm = s['llm_model'] as String?;
        _sttProvider = (s['stt_provider'] as String?) ?? 'openai_compat';
        _sttUrl.text = (s['stt_base_url'] as String?) ?? '';
        _sttModel.text = (s['stt_model'] as String?) ?? '';
        _sttLang.text = (s['stt_language'] as String?) ?? '';
        _whisperModel = s['whisper_model'] as String?;
        _whisperPath.text = (s['whisper_model_path'] as String?) ?? '';
        _whisperGpu = s['whisper_gpu'] != false;
        _gpuDevice.text = '${(s['gpu_device'] as num?)?.toInt() ?? 0}';
        _brollDir.text = (s['broll_dir'] as String?) ?? '';
        final tr = (s['tracking'] as Map?)?.cast<String, dynamic>() ?? const {};
        _trackMethod = (tr['method'] as String?) ?? 'auto';
        _faceModelPath.text = (tr['face_model'] as String?) ?? '';
        _trackFps = (tr['fps'] as num?)?.toDouble() ?? 2;
        _minFace = (tr['min_face'] as num?)?.toDouble() ?? 0.025;
        _faceThreshold = (tr['threshold'] as num?)?.toDouble() ?? 2;
        _trackSmoothing = (tr['smoothing'] as num?)?.toDouble();
        _trackZoom = (tr['zoom'] as num?)?.toDouble();
        _ytId.text = (s['youtube_client_id'] as String?) ?? '';
        _hasYtSecret = j['has_youtube_secret'] == true;
        _hasYtRefresh = j['has_youtube_refresh'] == true;
        _hasToken = ((j['has_platform_token'] as Map?) ?? {}).cast();
        _igUser.text = (s['instagram_user_id'] as String?) ?? '';
        _liAuthor.text = (s['linkedin_author'] as String?) ?? '';
        _hasStt = j['has_stt_key'] == true;
        _hasPexels = j['has_pexels_key'] == true;
        _hasPixabay = j['has_pixabay_key'] == true;
        _loaded = true;
      });
    } catch (e) {
      setState(() {
        _error = describeError(e);
        _loaded = true;
      });
    }
    unawaited(_loadWhisper());
  }

  /// GPU list comes from the whisper helper; can take a moment on first open, so it fills in later.
  Future<void> _loadWhisper() async {
    try {
      final w = jsonDecode(await api.studioWhisperStatus()) as Map;
      if (!mounted) return;
      setState(() {
        _whisperBackend = w['gpu'] as String?;
        final gd = w['gpu_devices'];
        if (gd is Map && gd['devices'] is List) {
          _gpuDevices = [
            for (final d in gd['devices'] as List) (d as Map).cast<String, dynamic>(),
          ];
        }
      });
    } catch (_) {}
  }

  // Subject tracking (Studio settings `tracking`).
  String _trackMethod = 'auto';
  final _faceModelPath = TextEditingController();
  double _trackFps = 2;
  double _minFace = 0.025;
  double _faceThreshold = 2;
  double? _trackSmoothing;
  double? _trackZoom;

  /// GPU backend of the whisper helper (`vulkan`, ...), from the whisper status.
  String? _whisperBackend;

  /// GPUs the whisper helper sees (`device` is 0-based; settings store device+1, 0 = auto).
  List<Map<String, dynamic>> _gpuDevices = const [];

  String _gpuNote() => _whisperBackend == null
      ? 'This install transcribes on the CPU (the helper has no GPU backend).'
      : 'Runs on ${_whisperBackend!} when available; falls back to the CPU on its own.';

  String? _n(TextEditingController c) =>
      c.text.trim().isEmpty ? null : c.text.trim();

  Future<void> _save() async {
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      await api.studioSaveSettings(
        settingsJson: jsonEncode({
          'llm_model': _llm,
          'stt_provider': _sttProvider,
          'stt_base_url': _n(_sttUrl),
          'stt_model': _n(_sttModel),
          'stt_language': _n(_sttLang),
          'whisper_model': _whisperModel,
          'whisper_model_path': _n(_whisperPath),
          'whisper_gpu': _whisperGpu,
          'gpu_device': int.tryParse(_gpuDevice.text.trim()) ?? 0,
          'broll_dir': _n(_brollDir),
          'tracking': {
            'method': _trackMethod,
            'face_model': _n(_faceModelPath),
            'fps': _trackFps,
            'min_face': _minFace,
            'threshold': _faceThreshold,
            'smoothing': _trackSmoothing,
            'zoom': _trackZoom,
          },
          'youtube_client_id': _n(_ytId),
          'instagram_user_id': _n(_igUser),
          'linkedin_author': _n(_liAuthor),
        }),
      );
      for (final (which, c) in [
        ('stt', _sttKey),
        ('pexels', _pexels),
        ('pixabay', _pixabay),
        ('youtube_secret', _ytSecret),
        ('youtube_refresh', _ytRefresh),
      ]) {
        if (c.text.trim().isNotEmpty) {
          await api.studioSetKey(which: which, key: c.text.trim());
        }
      }
      for (final e in _tokens.entries) {
        if (e.value.text.trim().isNotEmpty) {
          await api.studioSetKey(which: e.key, key: e.value.text.trim());
        }
      }
      if (mounted) Navigator.pop(context);
    } catch (e) {
      setState(() {
        _saving = false;
        _error = describeError(e);
      });
    }
  }

  Future<void> _clearKey(String which) async {
    await api.studioSetKey(which: which, key: null);
    await _load();
  }

  @override
  Widget build(BuildContext context) {
    final models = ref.watch(modelListProvider).value?.models ?? const [];
    final hk = context.hk;
    final refs = [for (final m in models) m.reference];
    return AlertDialog(
      title: const Text('Studio settings'),
      content: SizedBox(
        width: 520,
        child: !_loaded
            ? const SizedBox(
                height: 120,
                child: Center(child: CircularProgressIndicator()),
              )
            : SingleChildScrollView(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const _H('Tools'),
                    const ToolsPanel(),
                    const _H('AI model (highlights, scoring, copy, B-roll)'),
                    DropdownButtonFormField<String>(
                      initialValue: refs.contains(_llm) ? _llm : null,
                      isExpanded: true,
                      decoration: InputDecoration(
                        border: const OutlineInputBorder(),
                        isDense: true,
                        hintText: refs.isEmpty
                            ? 'Add a provider on the Models tab first'
                            : 'Choose a model',
                        helperText: _llm != null && !refs.contains(_llm)
                            ? 'Saved: $_llm'
                            : null,
                      ),
                      items: [
                        for (final r in refs)
                          DropdownMenuItem(
                            value: r,
                            child: Text(r, overflow: TextOverflow.ellipsis),
                          ),
                      ],
                      onChanged: (v) => setState(() => _llm = v),
                    ),
                    const _H('Speech to text'),
                    SegmentedButton<String>(
                      showSelectedIcon: false,
                      segments: const [
                        ButtonSegment(
                          value: 'openai_compat',
                          label: Text('API / server'),
                        ),
                        ButtonSegment(
                          value: 'whisper_local',
                          label: Text('Local whisper.cpp'),
                        ),
                      ],
                      selected: {_sttProvider},
                      onSelectionChanged: (v) =>
                          setState(() => _sttProvider = v.first),
                    ),
                    const SizedBox(height: HkTokens.s2),
                    if (_sttProvider == 'openai_compat') ...[
                      _Field(
                        _sttUrl,
                        'Base URL',
                        'https://api.openai.com/v1  (Groq, local servers work too)',
                      ),
                      _Field(_sttModel, 'Model', 'whisper-1'),
                      _KeyField(
                        controller: _sttKey,
                        label: 'API key',
                        isSet: _hasStt,
                        onClear: () => _clearKey('stt'),
                      ),
                    ] else ...[
                      WhisperModelsPicker(
                        selected: _whisperModel,
                        onSelected: (v) => setState(() => _whisperModel = v),
                      ),
                      _Field(
                        _whisperPath,
                        'Or your own ggml file (optional)',
                        '/path/to/ggml-base.bin',
                      ),
                      Row(
                        children: [
                          Expanded(
                            child: SwitchListTile(
                              contentPadding: EdgeInsets.zero,
                              title: const Text('Use the GPU'),
                              subtitle: Text(
                                _gpuNote(),
                                style: TextStyle(color: hk.textMuted),
                              ),
                              value: _whisperGpu,
                              onChanged: (v) => setState(() => _whisperGpu = v),
                            ),
                          ),
                          if (_whisperGpu)
                            SizedBox(
                              width: 260,
                              child: DropdownButtonFormField<String>(
                                initialValue: _gpuDevice.text.isEmpty
                                    ? '0'
                                    : _gpuDevice.text,
                                isExpanded: true,
                                decoration: const InputDecoration(
                                  labelText: 'GPU',
                                  isDense: true,
                                  border: OutlineInputBorder(),
                                ),
                                items: [
                                  const DropdownMenuItem(
                                    value: '0',
                                    child: Text('Auto (best discrete GPU)'),
                                  ),
                                  for (final d in _gpuDevices)
                                    DropdownMenuItem(
                                      value:
                                          '${(d['device'] as num).toInt() + 1}',
                                      child: Text(
                                        '${d['description'] ?? d['name']}${d['integrated'] == true ? ' (integrated)' : ''}',
                                        overflow: TextOverflow.ellipsis,
                                      ),
                                    ),
                                  if (_gpuDevices.isEmpty &&
                                      _gpuDevice.text != '0')
                                    DropdownMenuItem(
                                      value: _gpuDevice.text,
                                      child: Text('Device ${_gpuDevice.text}'),
                                    ),
                                ],
                                onChanged: (v) =>
                                    setState(() => _gpuDevice.text = v ?? '0'),
                              ),
                            ),
                        ],
                      ),
                    ],
                    _Field(
                      _sttLang,
                      'Language (optional)',
                      'en, tr, … empty = detect',
                    ),
                    const _H('B-roll sources'),
                    _KeyField(
                      controller: _pexels,
                      label: 'Pexels API key',
                      isSet: _hasPexels,
                      onClear: () => _clearKey('pexels'),
                    ),
                    _KeyField(
                      controller: _pixabay,
                      label: 'Pixabay API key',
                      isSet: _hasPixabay,
                      onClear: () => _clearKey('pixabay'),
                    ),
                    Row(
                      children: [
                        Expanded(
                          child: _Field(
                            _brollDir,
                            'Your own footage folder',
                            'optional',
                          ),
                        ),
                        IconButton(
                          tooltip: 'Choose folder',
                          icon: const Icon(Icons.folder_open),
                          onPressed: () async {
                            final d = await FilePicker.getDirectoryPath(
                              dialogTitle: 'B-roll folder',
                            );
                            if (d != null) setState(() => _brollDir.text = d);
                          },
                        ),
                      ],
                    ),
                    const _H('YouTube upload'),
                    _Field(
                      _ytId,
                      'OAuth client id',
                      'from Google Cloud Console (YouTube Data API v3)',
                    ),
                    _KeyField(
                      controller: _ytSecret,
                      label: 'OAuth client secret',
                      isSet: _hasYtSecret,
                      onClear: () => _clearKey('youtube_secret'),
                    ),
                    _KeyField(
                      controller: _ytRefresh,
                      label: 'Refresh token',
                      isSet: _hasYtRefresh,
                      onClear: () => _clearKey('youtube_refresh'),
                    ),
                    const _H('Publishing (official APIs)'),
                    _KeyField(
                      controller: _tokens['tiktok']!,
                      label: 'TikTok access token (video.publish)',
                      isSet: _hasToken['tiktok'] == true,
                      onClear: () => _clearKey('tiktok'),
                    ),
                    _KeyField(
                      controller: _tokens['instagram_reels']!,
                      label: 'Instagram Graph access token',
                      isSet: _hasToken['instagram_reels'] == true,
                      onClear: () => _clearKey('instagram_reels'),
                    ),
                    _Field(
                      _igUser,
                      'Instagram business account id',
                      'ig-user-id from Graph API Explorer',
                    ),
                    _KeyField(
                      controller: _tokens['x']!,
                      label:
                          'X OAuth 2.0 user token (tweet.write, media.write)',
                      isSet: _hasToken['x'] == true,
                      onClear: () => _clearKey('x'),
                    ),
                    _KeyField(
                      controller: _tokens['linkedin']!,
                      label: 'LinkedIn access token (w_member_social)',
                      isSet: _hasToken['linkedin'] == true,
                      onClear: () => _clearKey('linkedin'),
                    ),
                    _Field(
                      _liAuthor,
                      'LinkedIn author URN',
                      'urn:li:person:… or urn:li:organization:…',
                    ),
                    const _H('Speaker tracking'),
                    Row(
                      children: [
                        OutlinedButton.icon(
                          onPressed: () async {
                            setState(() => _faceStatus = 'Downloading…');
                            try {
                              final ok = await api.studioPrepareFaceModel();
                              setState(
                                () => _faceStatus = ok
                                    ? 'Face model ready'
                                    : 'Not available in this build',
                              );
                            } catch (e) {
                              setState(() => _faceStatus = describeError(e));
                            }
                          },
                          icon: const Icon(
                            Icons.face_retouching_natural,
                            size: 18,
                          ),
                          label: const Text('Download face model'),
                        ),
                        const SizedBox(width: HkTokens.s2),
                        Expanded(
                          child: Text(
                            _faceStatus ??
                                'Without it, framing follows motion.',
                            style: TextStyle(color: hk.textMuted),
                          ),
                        ),
                      ],
                    ),
                    const SizedBox(height: HkTokens.s2),
                    DropdownButtonFormField<String>(
                      initialValue: _trackMethod,
                      decoration: const InputDecoration(
                        labelText: 'Tracking method',
                        isDense: true,
                        border: OutlineInputBorder(),
                      ),
                      items: const [
                        DropdownMenuItem(
                          value: 'auto',
                          child: Text(
                            'Auto (faces when the model is there, else motion)',
                          ),
                        ),
                        DropdownMenuItem(
                          value: 'face',
                          child: Text('Faces (SeetaFace model)'),
                        ),
                        DropdownMenuItem(
                          value: 'motion',
                          child: Text('Motion (no model)'),
                        ),
                        DropdownMenuItem(
                          value: 'center',
                          child: Text('Fixed center crop'),
                        ),
                      ],
                      onChanged: (v) =>
                          setState(() => _trackMethod = v ?? 'auto'),
                    ),
                    const SizedBox(height: HkTokens.s2),
                    Row(
                      children: [
                        Expanded(
                          child: _Field(
                            _faceModelPath,
                            'Own face model file (optional)',
                            'SeetaFace .bin; empty = downloaded default',
                          ),
                        ),
                        IconButton(
                          tooltip: 'Choose model file',
                          icon: const Icon(Icons.folder_open),
                          onPressed: () async {
                            final r = await FilePicker.pickFiles(
                              type: FileType.any,
                              dialogTitle: 'Face model',
                            );
                            final path = r.firstOrNull?.path;
                            if (path != null) {
                              setState(() => _faceModelPath.text = path);
                            }
                          },
                        ),
                      ],
                    ),
                    Text(
                      'Frames analysed per second: ${_trackFps.toStringAsFixed(1)} (higher = follows faster, slower to compute)',
                    ),
                    Slider(
                      value: _trackFps.clamp(0.5, 8),
                      min: 0.5,
                      max: 8,
                      divisions: 15,
                      onChanged: (v) => setState(() => _trackFps = v),
                    ),
                    Text(
                      'Smallest face: ${(_minFace * 100).toStringAsFixed(1)}% of frame width',
                    ),
                    Slider(
                      value: _minFace.clamp(0.01, 0.2),
                      min: 0.01,
                      max: 0.2,
                      onChanged: (v) => setState(() => _minFace = v),
                    ),
                    Text(
                      'Detection threshold: ${_faceThreshold.toStringAsFixed(1)} (higher = fewer false faces)',
                    ),
                    Slider(
                      value: _faceThreshold.clamp(0.5, 6),
                      min: 0.5,
                      max: 6,
                      onChanged: (v) => setState(() => _faceThreshold = v),
                    ),
                    Text(
                      'Camera smoothing: ${_trackSmoothing == null ? 'per clip' : _trackSmoothing!.toStringAsFixed(2)}',
                    ),
                    Slider(
                      value: (_trackSmoothing ?? 0.5).clamp(0, 1),
                      onChanged: (v) => setState(() => _trackSmoothing = v),
                    ),
                    Text(
                      'Zoom: ${_trackZoom == null ? 'per clip' : '${_trackZoom!.toStringAsFixed(2)}×'}',
                    ),
                    Slider(
                      value: (_trackZoom ?? 1).clamp(1, 2),
                      min: 1,
                      max: 2,
                      onChanged: (v) => setState(() => _trackZoom = v),
                    ),
                    if (_error != null)
                      Padding(
                        padding: const EdgeInsets.only(top: HkTokens.s3),
                        child: Text(_error!, style: TextStyle(color: hk.err)),
                      ),
                  ],
                ),
              ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(context),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _saving || !_loaded ? null : _save,
          child: Text(_saving ? 'Saving…' : 'Save'),
        ),
      ],
    );
  }
}

class _H extends StatelessWidget {
  const _H(this.t);
  final String t;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(top: HkTokens.s4, bottom: HkTokens.s2),
    child: Text(t, style: Theme.of(context).textTheme.titleSmall),
  );
}

class _Field extends StatelessWidget {
  const _Field(this.c, this.label, this.hint);
  final TextEditingController c;
  final String label;
  final String hint;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: HkTokens.s2),
    child: TextField(
      controller: c,
      decoration: InputDecoration(
        labelText: label,
        hintText: hint,
        isDense: true,
        border: const OutlineInputBorder(),
      ),
    ),
  );
}

class _KeyField extends StatelessWidget {
  const _KeyField({
    required this.controller,
    required this.label,
    required this.isSet,
    required this.onClear,
  });
  final TextEditingController controller;
  final String label;
  final bool isSet;
  final VoidCallback onClear;
  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: HkTokens.s2),
    child: TextField(
      controller: controller,
      obscureText: true,
      decoration: InputDecoration(
        labelText: label,
        hintText: isSet ? 'Key set (type to replace)' : 'Not set',
        isDense: true,
        border: const OutlineInputBorder(),
        suffixIcon: isSet
            ? IconButton(
                tooltip: 'Remove key',
                icon: const Icon(Icons.delete_outline),
                onPressed: onClear,
              )
            : null,
      ),
    ),
  );
}
