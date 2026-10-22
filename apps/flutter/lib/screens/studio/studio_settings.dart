import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';
import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import 'studio_providers.dart';

Future<void> showStudioSettings(BuildContext context) => showDialog<void>(context: context, builder: (_) => const _StudioSettingsDialog());

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
  String? _llm;
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
    for (final c in [_sttUrl, _sttModel, _sttLang, _whisperPath, _sttKey, _pexels, _pixabay, _brollDir]) {
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
        _whisperPath.text = (s['whisper_model_path'] as String?) ?? '';
        _brollDir.text = (s['broll_dir'] as String?) ?? '';
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
  }

  String? _n(TextEditingController c) => c.text.trim().isEmpty ? null : c.text.trim();

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
          'whisper_model_path': _n(_whisperPath),
          'broll_dir': _n(_brollDir),
        }),
      );
      for (final (which, c) in [('stt', _sttKey), ('pexels', _pexels), ('pixabay', _pixabay)]) {
        if (c.text.trim().isNotEmpty) await api.studioSetKey(which: which, key: c.text.trim());
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
    final tools = ref.watch(studioToolsProvider).value;
    final hk = context.hk;
    final refs = [for (final m in models) m.reference];
    return AlertDialog(
      title: const Text('Studio settings'),
      content: SizedBox(
        width: 520,
        child: !_loaded
            ? const SizedBox(height: 120, child: Center(child: CircularProgressIndicator()))
            : SingleChildScrollView(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    if (tools != null && tools['ffmpeg'] != true)
                      Container(
                        padding: const EdgeInsets.all(HkTokens.s2),
                        margin: const EdgeInsets.only(bottom: HkTokens.s3),
                        decoration: BoxDecoration(color: hk.warn.withValues(alpha: 0.15), borderRadius: BorderRadius.circular(HkTokens.radiusCard)),
                        child: const Text('ffmpeg was not found. Install it (or set HK_FFMPEG) to import, cut and render videos.'),
                      ),
                    const _H('AI model (highlights, scoring, copy, B-roll)'),
                    DropdownButtonFormField<String>(
                      initialValue: refs.contains(_llm) ? _llm : null,
                      isExpanded: true,
                      decoration: InputDecoration(border: const OutlineInputBorder(), isDense: true, hintText: refs.isEmpty ? 'Add a provider on the Models tab first' : 'Choose a model', helperText: _llm != null && !refs.contains(_llm) ? 'Saved: $_llm' : null),
                      items: [for (final r in refs) DropdownMenuItem(value: r, child: Text(r, overflow: TextOverflow.ellipsis))],
                      onChanged: (v) => setState(() => _llm = v),
                    ),
                    const _H('Speech to text'),
                    SegmentedButton<String>(
                      showSelectedIcon: false,
                      segments: const [
                        ButtonSegment(value: 'openai_compat', label: Text('API / server')),
                        ButtonSegment(value: 'whisper_local', label: Text('Local whisper.cpp')),
                      ],
                      selected: {_sttProvider},
                      onSelectionChanged: (v) => setState(() => _sttProvider = v.first),
                    ),
                    const SizedBox(height: HkTokens.s2),
                    if (_sttProvider == 'openai_compat') ...[
                      _Field(_sttUrl, 'Base URL', 'https://api.openai.com/v1  (Groq, local servers work too)'),
                      _Field(_sttModel, 'Model', 'whisper-1'),
                      _KeyField(controller: _sttKey, label: 'API key', isSet: _hasStt, onClear: () => _clearKey('stt')),
                    ] else
                      _Field(_whisperPath, 'ggml model file', '/path/to/ggml-base.bin (needs a build with local whisper)'),
                    _Field(_sttLang, 'Language (optional)', 'en, tr, … empty = detect'),
                    const _H('B-roll sources'),
                    _KeyField(controller: _pexels, label: 'Pexels API key', isSet: _hasPexels, onClear: () => _clearKey('pexels')),
                    _KeyField(controller: _pixabay, label: 'Pixabay API key', isSet: _hasPixabay, onClear: () => _clearKey('pixabay')),
                    Row(
                      children: [
                        Expanded(child: _Field(_brollDir, 'Your own footage folder', 'optional')),
                        IconButton(
                          tooltip: 'Choose folder',
                          icon: const Icon(Icons.folder_open),
                          onPressed: () async {
                            final d = await FilePicker.getDirectoryPath(dialogTitle: 'B-roll folder');
                            if (d != null) setState(() => _brollDir.text = d);
                          },
                        ),
                      ],
                    ),
                    const _H('Speaker tracking'),
                    Row(
                      children: [
                        OutlinedButton.icon(
                          onPressed: () async {
                            setState(() => _faceStatus = 'Downloading…');
                            try {
                              final ok = await api.studioPrepareFaceModel();
                              setState(() => _faceStatus = ok ? 'Face model ready' : 'Not available in this build');
                            } catch (e) {
                              setState(() => _faceStatus = describeError(e));
                            }
                          },
                          icon: const Icon(Icons.face_retouching_natural, size: 18),
                          label: const Text('Download face model'),
                        ),
                        const SizedBox(width: HkTokens.s2),
                        Expanded(child: Text(_faceStatus ?? 'Without it, framing follows motion.', style: TextStyle(color: hk.textMuted))),
                      ],
                    ),
                    if (_error != null) Padding(padding: const EdgeInsets.only(top: HkTokens.s3), child: Text(_error!, style: TextStyle(color: hk.err))),
                  ],
                ),
              ),
      ),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(onPressed: _saving || !_loaded ? null : _save, child: Text(_saving ? 'Saving…' : 'Save')),
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
    child: TextField(controller: c, decoration: InputDecoration(labelText: label, hintText: hint, isDense: true, border: const OutlineInputBorder())),
  );
}

class _KeyField extends StatelessWidget {
  const _KeyField({required this.controller, required this.label, required this.isSet, required this.onClear});
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
        suffixIcon: isSet ? IconButton(tooltip: 'Remove key', icon: const Icon(Icons.delete_outline), onPressed: onClear) : null,
      ),
    ),
  );
}
