import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/hk_gate.dart' as gate;
import '../../ffi/settings_providers.dart';
import '../../theme/tokens.dart';
import '../../widgets/hk_ui.dart';
import 'chat_providers.dart';
import 'model_picker.dart';

/// Per-chat run settings (playground style): model, preset, system prompt, sampling and, for
/// on-device models, the llama.cpp runtime profile. Changes save automatically.
class RunSettingsPane extends ConsumerStatefulWidget {
  const RunSettingsPane({super.key, required this.sessionId});

  final String sessionId;

  @override
  ConsumerState<RunSettingsPane> createState() => _RunSettingsPaneState();
}

const _presets = <String, Map<String, dynamic>>{
  'Precise': {'temperature': 0.2, 'top_p': 0.9},
  'Balanced': {'temperature': 0.7, 'top_p': 0.95},
  'Creative': {'temperature': 1.1, 'top_p': 1.0},
};

class _RunSettingsPaneState extends ConsumerState<RunSettingsPane> {
  Map<String, dynamic> _s = {};
  Map<String, dynamic> _profiles = {};
  final _system = TextEditingController();
  final _stop = TextEditingController();
  final _seed = TextEditingController();
  Timer? _debounce;
  Timer? _localDebounce;
  bool _loaded = false;
  String? _saved;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _debounce?.cancel();
    _localDebounce?.cancel();
    _system.dispose();
    _stop.dispose();
    _seed.dispose();
    super.dispose();
  }

  Future<void> _load() async {
    try {
      final v = await gate.hk('session.settings.get', {
        'session_id': widget.sessionId,
      });
      final p = await gate.hk('settings.local.get');
      if (!mounted) return;
      setState(() {
        _s = v is Map ? v.cast<String, dynamic>() : {};
        _profiles = p is Map ? p.cast<String, dynamic>() : {};
        _system.text = (_s['system'] as String?) ?? '';
        _stop.text = ((_s['stop'] as List?) ?? const []).join(', ');
        _seed.text = _s['seed']?.toString() ?? '';
        _loaded = true;
      });
    } catch (_) {
      if (mounted) setState(() => _loaded = true);
    }
  }

  void _set(String k, Object? v) {
    setState(() => v == null ? _s.remove(k) : _s[k] = v);
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 500), _save);
  }

  /// Text fields are read at save time; just schedule one.
  void _touch() {
    _debounce?.cancel();
    _debounce = Timer(const Duration(milliseconds: 600), _save);
  }

  Future<void> _save() async {
    final stops = [
      for (final s in _stop.text.split(','))
        if (s.trim().isNotEmpty) s.trim(),
    ];
    final out = {..._s};
    stops.isEmpty ? out.remove('stop') : out['stop'] = stops;
    final seed = int.tryParse(_seed.text.trim());
    seed == null ? out.remove('seed') : out['seed'] = seed;
    final sys = _system.text.trim();
    sys.isEmpty ? out.remove('system') : out['system'] = sys;
    try {
      await gate.hk('session.settings.save', {
        'session_id': widget.sessionId,
        'settings': out,
      });
      if (mounted) setState(() => _saved = 'Saved');
    } catch (e) {
      if (mounted) setState(() => _saved = describeError(e));
    }
  }

  String? _model() {
    final picked = ref.watch(composerModelProvider);
    return picked ??
        ref
            .watch(sessionListProvider)
            .value
            ?.where((s) => s.id == widget.sessionId)
            .firstOrNull
            ?.model;
  }

  Map<String, dynamic> _profile(String id) =>
      ((_profiles[id] as Map?) ?? const {}).cast<String, dynamic>();

  void _setLocal(String id, String k, Object? v) {
    final p = {..._profile(id)};
    v == null ? p.remove(k) : p[k] = v;
    setState(() => _profiles = {..._profiles, id: p});
    _localDebounce?.cancel();
    _localDebounce = Timer(
      const Duration(milliseconds: 500),
      () => gate
          .hk('settings.local.save', {'profiles': _profiles})
          .catchError((_) => null),
    );
  }

  double? _d(String k) => (_s[k] as num?)?.toDouble();

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    if (!_loaded) return const Center(child: CircularProgressIndicator());
    final model = _model();
    final models = ref.watch(modelListProvider).value?.models ?? const [];
    final name =
        models.where((m) => m.reference == model).firstOrNull?.name ??
        model?.split('/').last ??
        'Default model';
    final local = model != null && model.startsWith('local/');
    final localId = local ? model.substring(6) : null;
    final activePreset = _presets.entries
        .where(
          (e) =>
              e.value['temperature'] == _s['temperature'] &&
              e.value['top_p'] == _s['top_p'],
        )
        .firstOrNull
        ?.key;
    return ListView(
      padding: const EdgeInsets.fromLTRB(
        HkTokens.s4,
        HkTokens.s1,
        HkTokens.s4,
        HkTokens.s6,
      ),
      children: [
        const SectionLabel('Model'),
        HoverCard(
          padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 10),
          onTap: () => showModelPicker(context),
          child: Row(
            children: [
              Icon(
                local ? Icons.memory : Icons.cloud_outlined,
                size: 18,
                color: hk.accent,
              ),
              const SizedBox(width: HkTokens.s2),
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(
                      name,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 13,
                        fontWeight: FontWeight.w600,
                      ),
                    ),
                    Text(
                      model ?? 'instance default',
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(fontSize: 11, color: hk.textFaint),
                    ),
                  ],
                ),
              ),
              Icon(Icons.unfold_more, size: 18, color: hk.textMuted),
            ],
          ),
        ),
        const SectionLabel('Preset'),
        SegmentedButton<String>(
          showSelectedIcon: false,
          emptySelectionAllowed: true,
          segments: [
            for (final k in _presets.keys)
              ButtonSegment(value: k, label: Text(k)),
          ],
          selected: {?activePreset},
          onSelectionChanged: (v) {
            if (v.isEmpty) return;
            _presets[v.first]!.forEach(_set);
          },
        ),
        SectionLabel(
          'System prompt',
          trailing: Tooltip(
            message: 'Used from the first message of this chat',
            child: Icon(Icons.info_outline, size: 14, color: hk.textFaint),
          ),
        ),
        TextField(
          controller: _system,
          minLines: 3,
          maxLines: 8,
          style: const TextStyle(fontSize: 13),
          decoration: const InputDecoration(
            hintText: 'You are a helpful assistant…',
          ),
          onChanged: (_) => _touch(),
        ),
        const SectionLabel('Sampling'),
        SliderRow(
          label: 'Temperature',
          help: 'Higher = more random',
          value: _d('temperature'),
          fallback: 0.7,
          min: 0,
          max: 2,
          divisions: 40,
          onChanged: (v) => _set('temperature', _round(v)),
        ),
        SliderRow(
          label: 'Top P',
          value: _d('top_p'),
          fallback: 0.95,
          min: 0,
          max: 1,
          divisions: 20,
          onChanged: (v) => _set('top_p', _round(v)),
        ),
        SliderRow(
          label: 'Top K',
          value: _d('top_k'),
          fallback: 40,
          min: 0,
          max: 200,
          divisions: 200,
          onChanged: (v) => _set('top_k', v?.round()),
        ),
        SliderRow(
          label: 'Min P',
          value: _d('min_p'),
          fallback: 0,
          min: 0,
          max: 0.5,
          divisions: 50,
          onChanged: (v) => _set('min_p', _round(v)),
        ),
        SliderRow(
          label: 'Max output tokens',
          value: _d('max_output_tokens'),
          fallback: 1024,
          min: 64,
          max: 32768,
          divisions: 511,
          onChanged: (v) => _set('max_output_tokens', v?.round()),
        ),
        ExpansionTile(
          tilePadding: EdgeInsets.zero,
          childrenPadding: EdgeInsets.zero,
          title: Text(
            'Advanced sampling',
            style: TextStyle(fontSize: 12, color: hk.textMuted),
          ),
          children: [
            SliderRow(
              label: 'Repetition penalty',
              value: _d('repetition_penalty'),
              fallback: 1,
              min: 1,
              max: 2,
              divisions: 40,
              onChanged: (v) => _set('repetition_penalty', _round(v)),
            ),
            SliderRow(
              label: 'Presence penalty',
              value: _d('presence_penalty'),
              fallback: 0,
              min: -2,
              max: 2,
              divisions: 40,
              onChanged: (v) => _set('presence_penalty', _round(v)),
            ),
            SliderRow(
              label: 'Frequency penalty',
              value: _d('frequency_penalty'),
              fallback: 0,
              min: -2,
              max: 2,
              divisions: 40,
              onChanged: (v) => _set('frequency_penalty', _round(v)),
            ),
            const SizedBox(height: HkTokens.s2),
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _seed,
                    keyboardType: TextInputType.number,
                    decoration: const InputDecoration(labelText: 'Seed'),
                    onChanged: (_) => _touch(),
                  ),
                ),
                const SizedBox(width: HkTokens.s2),
                Expanded(
                  flex: 2,
                  child: TextField(
                    controller: _stop,
                    decoration: const InputDecoration(
                      labelText: 'Stop sequences',
                      hintText: 'comma separated',
                    ),
                    onChanged: (_) => _touch(),
                  ),
                ),
              ],
            ),
          ],
        ),
        const SectionLabel('Reasoning'),
        SegmentedButton<String>(
          showSelectedIcon: false,
          segments: const [
            ButtonSegment(value: 'default', label: Text('Auto')),
            ButtonSegment(value: 'off', label: Text('Off')),
            ButtonSegment(value: 'low', label: Text('Low')),
            ButtonSegment(value: 'medium', label: Text('Med')),
            ButtonSegment(value: 'high', label: Text('High')),
          ],
          selected: {
            _s['thinking_budget'] == 0
                ? 'off'
                : (_s['reasoning_effort'] as String?) ?? 'default',
          },
          onSelectionChanged: (v) {
            final x = v.first;
            _set('thinking_budget', x == 'off' ? 0 : null);
            _set('reasoning_effort', x == 'default' || x == 'off' ? null : x);
          },
        ),
        if (localId != null) ..._localSection(context, localId),
        const SizedBox(height: HkTokens.s4),
        Row(
          children: [
            TextButton.icon(
              onPressed: () {
                setState(() {
                  _s = {};
                  _system.clear();
                  _stop.clear();
                  _seed.clear();
                });
                _save();
              },
              icon: const Icon(Icons.restart_alt, size: 16),
              label: const Text('Reset to defaults'),
            ),
            const Spacer(),
            if (_saved != null)
              Text(
                _saved!,
                style: TextStyle(fontSize: 11, color: hk.textFaint),
              ),
          ],
        ),
      ],
    );
  }

  List<Widget> _localSection(BuildContext context, String id) {
    final hk = context.hk;
    final p = _profile(id);
    double? d(String k) => (p[k] as num?)?.toDouble();
    return [
      SectionLabel(
        'On-device runtime',
        trailing: Pill('llama.cpp', color: hk.accent),
      ),
      Text(
        'Applies to $id on the next message. GPU layers, mmap and mlock reload the model.',
        style: TextStyle(fontSize: 11, color: hk.textFaint),
      ),
      const SizedBox(height: HkTokens.s2),
      SliderRow(
        label: 'Context length',
        value: d('ctx'),
        fallback: 8192,
        min: 1024,
        max: 131072,
        divisions: 127,
        format: (v) => '${(v / 1024).round()}K',
        onChanged: (v) =>
            _setLocal(id, 'ctx', v == null ? null : (v / 1024).round() * 1024),
      ),
      SliderRow(
        label: 'GPU layers',
        help: '-1 = all layers on the GPU, 0 = CPU only',
        value: d('gpu_layers'),
        fallback: -1,
        min: -1,
        max: 100,
        divisions: 101,
        format: (v) => v < 0 ? 'All' : (v == 0 ? 'CPU' : '${v.round()}'),
        onChanged: (v) => _setLocal(id, 'gpu_layers', v?.round()),
      ),
      SliderRow(
        label: 'CPU threads',
        value: d('threads'),
        fallback: 4,
        min: 1,
        max: 32,
        divisions: 31,
        onChanged: (v) => _setLocal(id, 'threads', v?.round()),
      ),
      SliderRow(
        label: 'Batch size',
        value: d('batch'),
        fallback: 512,
        min: 64,
        max: 4096,
        divisions: 63,
        onChanged: (v) => _setLocal(id, 'batch', v?.round()),
      ),
      SettingRow(
        label: 'Flash attention',
        child: _Seg(
          values: const ['auto', 'on', 'off'],
          value: (p['flash_attn'] as String?) ?? 'auto',
          onChanged: (v) => _setLocal(id, 'flash_attn', v == 'auto' ? null : v),
        ),
      ),
      SettingRow(
        label: 'KV cache',
        help: 'Quantized cache fits longer contexts',
        child: _Seg(
          values: const ['f16', 'q8_0', 'q4_0'],
          value: (p['kv_type'] as String?) ?? 'f16',
          onChanged: (v) => _setLocal(id, 'kv_type', v == 'f16' ? null : v),
        ),
      ),
      SettingRow(
        label: 'Memory-map model file',
        child: Switch(
          value: (p['mmap'] as bool?) ?? true,
          onChanged: (v) => _setLocal(id, 'mmap', v ? null : false),
        ),
      ),
      SettingRow(
        label: 'Lock in RAM (mlock)',
        child: Switch(
          value: (p['mlock'] as bool?) ?? false,
          onChanged: (v) => _setLocal(id, 'mlock', v ? true : null),
        ),
      ),
    ];
  }
}

double? _round(double? v) => v == null ? null : (v * 100).roundToDouble() / 100;

class _Seg extends StatelessWidget {
  const _Seg({
    required this.values,
    required this.value,
    required this.onChanged,
  });

  final List<String> values;
  final String value;
  final ValueChanged<String> onChanged;

  @override
  Widget build(BuildContext context) => SegmentedButton<String>(
    showSelectedIcon: false,
    style: const ButtonStyle(visualDensity: VisualDensity(vertical: -3)),
    segments: [for (final v in values) ButtonSegment(value: v, label: Text(v))],
    selected: {value},
    onSelectionChanged: (s) => onChanged(s.first),
  );
}
