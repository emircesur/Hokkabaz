import 'package:flutter/material.dart';

import '../../ffi/hk_gate.dart';
import '../../theme/tokens.dart';
import 'ai_common.dart';

/// AI settings (P9.24 sampling/reasoning/output/context/cost, P9.27 approvals, P9.25 local
/// runtime, generation engines).
class AiSettingsSection extends StatelessWidget {
  const AiSettingsSection({super.key});

  @override
  Widget build(BuildContext context) => const DefaultTabController(
    length: 4,
    child: Column(
      children: [
        TabBar(
          isScrollable: true,
          tabAlignment: TabAlignment.start,
          tabs: [
            Tab(text: 'Model behaviour'),
            Tab(text: 'Approvals'),
            Tab(text: 'Local runtime'),
            Tab(text: 'Generation'),
          ],
        ),
        Expanded(
          child: TabBarView(
            children: [_SamplingTab(), _ApprovalsTab(), _LocalTab(), _GenTab()],
          ),
        ),
      ],
    ),
  );
}

/// Number/text/bool field rows bound to one JSON map.
class _MapForm extends StatelessWidget {
  const _MapForm({
    required this.values,
    required this.fields,
    required this.onChanged,
  });

  final Map<String, dynamic> values;
  final List<(String, String, String)>
  fields; // key, label, type: num|int|text|bool|choice:a,b
  final VoidCallback onChanged;

  @override
  Widget build(BuildContext context) => Wrap(
    spacing: HkTokens.s3,
    runSpacing: HkTokens.s3,
    children: [
      for (final (key, label, type) in fields)
        SizedBox(
          width: 240,
          child: switch (type) {
            'bool' => SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: Text(label),
              value: values[key] == true,
              onChanged: (v) {
                values[key] = v;
                onChanged();
              },
            ),
            _ when type.startsWith('choice:') =>
              DropdownButtonFormField<String>(
                initialValue: values[key] as String?,
                decoration: InputDecoration(
                  labelText: label,
                  border: const OutlineInputBorder(),
                  isDense: true,
                ),
                items: [
                  const DropdownMenuItem<String>(
                    value: null,
                    child: Text('inherit'),
                  ),
                  for (final o in type.substring(7).split(','))
                    DropdownMenuItem<String>(value: o, child: Text(o)),
                ],
                onChanged: (v) {
                  if (v == null) {
                    values.remove(key);
                  } else {
                    values[key] = v;
                  }
                  onChanged();
                },
              ),
            _ => TextFormField(
              key: ValueKey('$key:${values[key]}'),
              initialValue: values[key]?.toString() ?? '',
              keyboardType: type == 'text' ? null : TextInputType.number,
              decoration: InputDecoration(
                labelText: label,
                hintText: 'inherit',
                border: const OutlineInputBorder(),
                isDense: true,
              ),
              onChanged: (t) {
                final v = t.trim();
                if (v.isEmpty) {
                  values.remove(key);
                } else if (type == 'int') {
                  values[key] = int.tryParse(v);
                } else if (type == 'num') {
                  values[key] = double.tryParse(v);
                } else {
                  values[key] = v;
                }
              },
            ),
          },
        ),
    ],
  );
}

class _SamplingTab extends StatefulWidget {
  const _SamplingTab();
  @override
  State<_SamplingTab> createState() => _SamplingTabState();
}

class _SamplingTabState extends State<_SamplingTab> {
  Map<String, dynamic> _global = {};
  List<Map<String, dynamic>> _presets = const [];

  static const _fields = <(String, String, String)>[
    ('temperature', 'Temperature', 'num'),
    ('top_p', 'Top P', 'num'),
    ('top_k', 'Top K', 'int'),
    ('min_p', 'Min P', 'num'),
    ('max_output_tokens', 'Max output tokens', 'int'),
    ('presence_penalty', 'Presence penalty', 'num'),
    ('frequency_penalty', 'Frequency penalty', 'num'),
    ('repetition_penalty', 'Repetition penalty', 'num'),
    ('seed', 'Seed', 'int'),
    ('reasoning_effort', 'Reasoning effort', 'choice:low,medium,high'),
    ('thinking_budget', 'Thinking budget (tokens)', 'int'),
    ('show_reasoning', 'Show reasoning', 'bool'),
    ('keep_reasoning_in_history', 'Keep reasoning in history', 'bool'),
    ('json_mode', 'JSON mode', 'bool'),
    ('auto_repair_retries', 'JSON auto-repair retries', 'int'),
    ('context_limit', 'Context limit (tokens)', 'int'),
    ('compaction_threshold', 'Compaction threshold (tokens)', 'int'),
    ('spill_kb', 'Spill tool output above (KB)', 'int'),
    ('memory_injection', 'Inject memory', 'bool'),
    ('rag_top_k', 'Knowledge top-k', 'int'),
    ('per_turn_cost_cap_usd', 'Cost cap per turn (USD)', 'num'),
  ];

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    final doc = await hk('settings.ai.get');
    final presets = await hkList('settings.ai.presets');
    if (!mounted) return;
    setState(() {
      _global = (doc is Map && doc['global'] is Map)
          ? (doc['global'] as Map).cast<String, dynamic>()
          : {};
      _presets = presets;
    });
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(HkTokens.s4),
    children: [
      Text('Presets', style: Theme.of(context).textTheme.titleMedium),
      const SizedBox(height: HkTokens.s2),
      Wrap(
        spacing: 8,
        children: [
          for (final p in _presets)
            ActionChip(
              label: Text(p['name'] as String? ?? ''),
              tooltip: p['description'] as String?,
              onPressed: () => setState(() {
                final s =
                    (p['settings'] as Map?)?.cast<String, dynamic>() ?? {};
                _global = {
                  ..._global,
                  for (final e in s.entries)
                    if (e.value != null) e.key: e.value,
                };
              }),
            ),
        ],
      ),
      const SizedBox(height: HkTokens.s4),
      Text(
        'Global defaults (providers, models, agents and sessions can override)',
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: HkTokens.s2),
      _MapForm(
        values: _global,
        fields: _fields,
        onChanged: () => setState(() {}),
      ),
      const SizedBox(height: HkTokens.s4),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton(
          onPressed: () => guarded(
            context,
            () => hk('settings.ai.save', {
              'settings': {'global': _global},
            }),
            ok: 'Saved',
          ),
          child: const Text('Save'),
        ),
      ),
    ],
  );
}

class _ApprovalsTab extends StatefulWidget {
  const _ApprovalsTab();
  @override
  State<_ApprovalsTab> createState() => _ApprovalsTabState();
}

class _ApprovalsTabState extends State<_ApprovalsTab> {
  static const _tools = [
    'bash',
    'write',
    'edit',
    'web_fetch',
    'checkpoint',
    'memory',
    'knowledge',
    'task',
    'studio_render',
    'studio_publish',
    'flow_run',
  ];
  static const _modes = ['build', 'plan'];
  Map<String, dynamic> _policy = {};
  bool _dryRun = false;

  @override
  void initState() {
    super.initState();
    hk('settings.approvals.get')
        .then((v) {
          if (!mounted || v is! Map) return;
          setState(() {
            _policy = ((v['rules'] as Map?) ?? {}).cast<String, dynamic>();
            _dryRun = v['dry_run'] == true;
          });
        })
        .catchError((_) {});
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(HkTokens.s4),
    children: [
      Text(
        'Per-tool approval policy',
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: HkTokens.s2),
      DataTable(
        columns: [
          const DataColumn(label: Text('Tool')),
          for (final m in _modes) DataColumn(label: Text(m)),
        ],
        rows: [
          for (final t in _tools)
            DataRow(
              cells: [
                DataCell(Text(t)),
                for (final m in _modes)
                  DataCell(
                    DropdownButton<String>(
                      value: (_policy['$t:$m'] as String?) ?? 'default',
                      underline: const SizedBox(),
                      items: const [
                        DropdownMenuItem(
                          value: 'default',
                          child: Text('default'),
                        ),
                        DropdownMenuItem(value: 'allow', child: Text('allow')),
                        DropdownMenuItem(value: 'ask', child: Text('ask')),
                        DropdownMenuItem(value: 'deny', child: Text('deny')),
                      ],
                      onChanged: (v) => setState(
                        () => v == null || v == 'default'
                            ? _policy.remove('$t:$m')
                            : _policy['$t:$m'] = v,
                      ),
                    ),
                  ),
              ],
            ),
        ],
      ),
      SwitchListTile(
        title: const Text('Dry-run mode'),
        subtitle: const Text(
          'Tools that change files or the outside world report what they would do instead.',
        ),
        value: _dryRun,
        onChanged: (v) => setState(() => _dryRun = v),
      ),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton(
          onPressed: () => guarded(
            context,
            () => hk('settings.approvals.save', {
              'policy': {'rules': _policy, 'dry_run': _dryRun},
            }),
            ok: 'Saved',
          ),
          child: const Text('Save'),
        ),
      ),
    ],
  );
}

class _LocalTab extends StatefulWidget {
  const _LocalTab();
  @override
  State<_LocalTab> createState() => _LocalTabState();
}

class _LocalTabState extends State<_LocalTab> {
  Map<String, dynamic> _p = {};
  final _model = TextEditingController();
  String? _bench;

  static const _fields = <(String, String, String)>[
    ('context_length', 'Context length', 'int'),
    ('gpu_layers', 'GPU layers', 'int'),
    ('threads', 'Threads', 'int'),
    ('batch_size', 'Batch size', 'int'),
    ('kv_cache_type', 'KV cache quantization', 'choice:f16,q8_0,q4_0'),
    ('flash_attention', 'Flash attention', 'bool'),
    ('mmap', 'mmap', 'bool'),
    ('mlock', 'mlock', 'bool'),
    ('keep_alive_minutes', 'Keep alive (minutes)', 'int'),
  ];

  @override
  void initState() {
    super.initState();
    hk('settings.local.get')
        .then((v) {
          if (mounted && v is Map) {
            setState(() => _p = v.cast<String, dynamic>());
          }
        })
        .catchError((_) {});
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(HkTokens.s4),
    children: [
      Text(
        'Local model runtime profile',
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: HkTokens.s2),
      _MapForm(values: _p, fields: _fields, onChanged: () => setState(() {})),
      const SizedBox(height: HkTokens.s3),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton(
          onPressed: () => guarded(
            context,
            () => hk('settings.local.save', {'profiles': _p}),
            ok: 'Saved',
          ),
          child: const Text('Save'),
        ),
      ),
      const Divider(height: 32),
      Text('Benchmark', style: Theme.of(context).textTheme.titleMedium),
      const SizedBox(height: HkTokens.s2),
      Row(
        children: [
          Expanded(
            child: TextField(
              controller: _model,
              decoration: const InputDecoration(
                labelText: 'Model (e.g. local/qwen3-4b or ollama/llama3.2)',
                border: OutlineInputBorder(),
                isDense: true,
              ),
            ),
          ),
          const SizedBox(width: HkTokens.s2),
          FilledButton.tonal(
            onPressed: () async {
              setState(() => _bench = 'running…');
              final r = await guarded(
                context,
                () => hkMap('local.benchmark', {'model': _model.text.trim()}),
              );
              if (mounted) {
                setState(
                  () => _bench = r == null
                      ? null
                      : (r['error'] ??
                                '${r['tokens_per_s']} tok/s · TTFT ${r['ttft_ms']} ms')
                            .toString(),
                );
              }
            },
            child: const Text('Run'),
          ),
        ],
      ),
      if (_bench != null)
        Padding(padding: const EdgeInsets.only(top: 8), child: Text(_bench!)),
    ],
  );
}

class _GenTab extends StatefulWidget {
  const _GenTab();
  @override
  State<_GenTab> createState() => _GenTabState();
}

class _GenTabState extends State<_GenTab> {
  Map<String, dynamic> _tts = {
    'engine': 'open_ai',
    'base_url': 'https://api.openai.com/v1',
  };
  Map<String, dynamic> _emb = {'engine': 'off'};
  Map<String, dynamic> _img = {
    'engine': 'open_ai',
    'base_url': 'https://api.openai.com/v1',
  };

  @override
  void initState() {
    super.initState();
    hk('genai.get')
        .then((v) {
          if (!mounted || v is! Map) return;
          setState(() {
            if (v['tts'] is Map) {
              _tts = (v['tts'] as Map).cast<String, dynamic>();
            }
            if (v['image'] is Map) {
              _img = (v['image'] as Map).cast<String, dynamic>();
            }
            if (v['embed'] is Map) {
              _emb = (v['embed'] as Map).cast<String, dynamic>();
            }
          });
        })
        .catchError((_) {});
  }

  @override
  Widget build(BuildContext context) => ListView(
    padding: const EdgeInsets.all(HkTokens.s4),
    children: [
      Text('Text to speech', style: Theme.of(context).textTheme.titleMedium),
      const SizedBox(height: HkTokens.s2),
      _MapForm(
        values: _tts,
        onChanged: () => setState(() {}),
        fields: _tts['engine'] == 'piper'
            ? const [
                ('engine', 'Engine', 'choice:open_ai,piper'),
                ('binary', 'Piper binary', 'text'),
                ('model', 'Voice model (.onnx)', 'text'),
              ]
            : const [
                ('engine', 'Engine', 'choice:open_ai,piper'),
                ('base_url', 'Base URL', 'text'),
                ('api_key', 'API key', 'text'),
                ('model', 'Model', 'text'),
                ('voice', 'Voice', 'text'),
              ],
      ),
      const SizedBox(height: HkTokens.s4),
      Text('Image generation', style: Theme.of(context).textTheme.titleMedium),
      const SizedBox(height: HkTokens.s2),
      _MapForm(
        values: _img,
        onChanged: () => setState(() {}),
        fields: _img['engine'] == 'comfy_ui'
            ? const [
                ('engine', 'Engine', 'choice:open_ai,comfy_ui'),
                ('base_url', 'ComfyUI URL', 'text'),
                ('checkpoint', 'Checkpoint file', 'text'),
              ]
            : const [
                ('engine', 'Engine', 'choice:open_ai,comfy_ui'),
                ('base_url', 'Base URL', 'text'),
                ('api_key', 'API key', 'text'),
                ('model', 'Model', 'text'),
              ],
      ),
      const SizedBox(height: HkTokens.s4),
      Text(
        'Embeddings (knowledge search)',
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: HkTokens.s1),
      Text(
        'With an embedding model, knowledge bases use hybrid search (keywords + meaning). Re-index after changing it.',
        style: TextStyle(fontSize: 12, color: context.hk.textMuted),
      ),
      const SizedBox(height: HkTokens.s2),
      _MapForm(
        values: _emb,
        onChanged: () => setState(() {}),
        fields: switch (_emb['engine']) {
          'ollama' => const [
            ('engine', 'Engine', 'choice:off,open_ai,ollama'),
            ('base_url', 'Ollama URL', 'text'),
            ('model', 'Model (e.g. nomic-embed-text)', 'text'),
          ],
          'open_ai' => const [
            ('engine', 'Engine', 'choice:off,open_ai,ollama'),
            ('base_url', 'Base URL', 'text'),
            ('api_key', 'API key', 'text'),
            ('model', 'Model', 'text'),
          ],
          _ => const [('engine', 'Engine', 'choice:off,open_ai,ollama')],
        },
      ),
      const SizedBox(height: HkTokens.s4),
      Align(
        alignment: Alignment.centerLeft,
        child: FilledButton(
          onPressed: () => guarded(
            context,
            () => hk('genai.save', {
              'settings': {
                'tts': _tts,
                'image': _img,
                'embed': (_emb['engine'] == null || _emb['engine'] == 'off')
                    ? null
                    : _emb,
              },
            }),
            ok: 'Saved',
          ),
          child: const Text('Save'),
        ),
      ),
    ],
  );
}
