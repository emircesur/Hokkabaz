import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../ffi/hk_gate.dart' as gate;
import '../../ffi/settings_providers.dart';
import '../../theme/tokens.dart';
import '../../widgets/hk_ui.dart';
import '../chat/chat_providers.dart'
    show agentStartersProvider, selectedSessionProvider;
import 'ai_common.dart';

/// Builtin tools an agent can be pointed at (the prompt asks it to prefer them).
const _toolChoices = [
  'read',
  'write',
  'edit',
  'glob',
  'grep',
  'bash',
  'web_fetch',
  'todo',
  'memory',
  'knowledge',
  'checkpoint',
];

const _avatars = ['🤖', '🧠', '🛠️', '✍️', '🔎', '📊', '🎬', '🧪', '🧭', '💬'];

/// Custom agents (P9.21): cards with start / edit / duplicate / delete and a full editor.
class AgentsSection extends ConsumerStatefulWidget {
  const AgentsSection({super.key});

  @override
  ConsumerState<AgentsSection> createState() => _AgentsSectionState();
}

class _AgentsSectionState extends ConsumerState<AgentsSection> {
  List<Map<String, dynamic>>? _items;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final v = await gate.hk('agents.list');
      if (mounted) {
        setState(
          () => _items = [
            for (final a in (v as List? ?? const []))
              (a as Map).cast<String, dynamic>(),
          ],
        );
      }
    } catch (_) {
      if (mounted) setState(() => _items = []);
    }
  }

  Future<void> _save(Map<String, dynamic> a) async {
    await guarded(
      context,
      () => gate.hk('agents.save', {'item': a}),
      ok: 'Saved',
    );
    await _load();
  }

  Future<void> _edit([Map<String, dynamic>? a]) async {
    final out = await showDialog<Map<String, dynamic>>(
      context: context,
      builder: (_) => _AgentEditor(initial: a),
    );
    if (out != null) await _save(out);
  }

  Future<void> _start(Map<String, dynamic> a) async {
    final info = await guarded(
      context,
      () => gate.hkMap('agents.start', {'id': a['id']}),
    );
    if (info == null || !mounted) return;
    final id = info['id'] as String?;
    final starters = ((a['starters'] as List?) ?? const []).cast<String>();
    if (id != null && starters.isNotEmpty) {
      ref.read(agentStartersProvider.notifier).set(id, starters);
    }
    ref.read(selectedSessionProvider.notifier).select(id);
    context.go('/chat');
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final items = _items;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        PageHeader(
          title: 'Agents',
          subtitle: 'Reusable assistants: persona, model, tools and sampling. Start a chat with one, or use it from the Agent node.',
          actions: [
            GradientButton(
              label: 'New agent',
              icon: Icons.add,
              onPressed: () => _edit(),
            ),
          ],
        ),
        Expanded(
          child: items == null
              ? const Center(child: CircularProgressIndicator())
              : items.isEmpty
              ? EmptyState(
                  icon: Icons.smart_toy_outlined,
                  title: 'No agents yet',
                  message: 'Create one, or install a ready-made agent from Templates.',
                  action: GradientButton(
                    label: 'New agent',
                    icon: Icons.add,
                    onPressed: () => _edit(),
                  ),
                )
              : GridView.builder(
                  padding: const EdgeInsets.all(HkTokens.s6),
                  gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                    maxCrossAxisExtent: 360,
                    mainAxisExtent: 180,
                    crossAxisSpacing: HkTokens.s3,
                    mainAxisSpacing: HkTokens.s3,
                  ),
                  itemCount: items.length,
                  itemBuilder: (context, i) {
                    final a = items[i];
                    final tools = (a['tools'] as List?)?.length ?? 0;
                    return HoverCard(
                      onTap: () => _edit(a),
                      child: Column(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Row(
                            children: [
                              Container(
                                width: 40,
                                height: 40,
                                alignment: Alignment.center,
                                decoration: BoxDecoration(
                                  color: hk.surface3,
                                  borderRadius: BorderRadius.circular(12),
                                ),
                                child: Text(
                                  (a['avatar'] as String?) ?? '🤖',
                                  style: const TextStyle(fontSize: 20),
                                ),
                              ),
                              const SizedBox(width: HkTokens.s3),
                              Expanded(
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      '${a['name'] ?? a['id']}',
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: const TextStyle(
                                        fontSize: 14,
                                        fontWeight: FontWeight.w600,
                                      ),
                                    ),
                                    Text(
                                      (a['model'] as String?)?.isNotEmpty ==
                                              true
                                          ? a['model'] as String
                                          : 'default model',
                                      maxLines: 1,
                                      overflow: TextOverflow.ellipsis,
                                      style: TextStyle(
                                        fontSize: 11,
                                        color: hk.textFaint,
                                      ),
                                    ),
                                  ],
                                ),
                              ),
                              PopupMenuButton<String>(
                                tooltip: 'More',
                                onSelected: (v) async {
                                  if (v == 'edit') _edit(a);
                                  if (v == 'dup') {
                                    await _save({
                                      ...a,
                                      'id':
                                          'agent_${DateTime.now().millisecondsSinceEpoch}',
                                      'name': '${a['name'] ?? 'Agent'} (copy)',
                                    });
                                  }
                                  if (v == 'delete' && context.mounted) {
                                    await guarded(
                                      context,
                                      () => gate.hk('agents.delete', {
                                        'id': a['id'],
                                      }),
                                    );
                                    await _load();
                                  }
                                },
                                itemBuilder: (_) => const [
                                  PopupMenuItem(
                                    value: 'edit',
                                    child: Text('Edit'),
                                  ),
                                  PopupMenuItem(
                                    value: 'dup',
                                    child: Text('Duplicate'),
                                  ),
                                  PopupMenuItem(
                                    value: 'delete',
                                    child: Text('Delete'),
                                  ),
                                ],
                              ),
                            ],
                          ),
                          const SizedBox(height: HkTokens.s3),
                          Expanded(
                            child: Text(
                              (a['description'] as String?)?.isNotEmpty == true
                                  ? a['description'] as String
                                  : (a['system_prompt'] as String? ?? ''),
                              maxLines: 3,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: 12,
                                color: hk.textMuted,
                              ),
                            ),
                          ),
                          Row(
                            children: [
                              Pill(
                                (a['mode'] as String?) == 'plan'
                                    ? 'Plan'
                                    : 'Build',
                                icon: Icons.build_outlined,
                              ),
                              const SizedBox(width: 6),
                              Pill(
                                '$tools tools',
                                icon: Icons.handyman_outlined,
                              ),
                              const Spacer(),
                              FilledButton.tonalIcon(
                                onPressed: () => _start(a),
                                icon: const Icon(Icons.chat_outlined, size: 16),
                                label: const Text('Chat'),
                              ),
                            ],
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
}

class _AgentEditor extends ConsumerStatefulWidget {
  const _AgentEditor({this.initial});

  final Map<String, dynamic>? initial;

  @override
  ConsumerState<_AgentEditor> createState() => _AgentEditorState();
}

class _AgentEditorState extends ConsumerState<_AgentEditor> {
  late final Map<String, dynamic> _a = {
    'id': 'agent_${DateTime.now().millisecondsSinceEpoch}',
    'avatar': '🤖',
    'mode': 'build',
    'tools': <String>[],
    ...?widget.initial,
  };
  late final _name = TextEditingController(text: _a['name'] as String? ?? '');
  late final _desc = TextEditingController(
    text: _a['description'] as String? ?? '',
  );
  late final _system = TextEditingController(
    text: _a['system_prompt'] as String? ?? '',
  );
  late final _starters = TextEditingController(
    text: ((_a['starters'] as List?) ?? const []).join('\n'),
  );

  Map<String, dynamic> get _settings =>
      ((_a['settings'] as Map?) ?? const {}).cast<String, dynamic>();

  void _setSetting(String k, Object? v) => setState(() {
    final s = {..._settings};
    v == null ? s.remove(k) : s[k] = v;
    _a['settings'] = s;
  });

  @override
  void dispose() {
    _name.dispose();
    _desc.dispose();
    _system.dispose();
    _starters.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final models = ref.watch(modelListProvider).value?.models ?? const [];
    final tools = ((_a['tools'] as List?) ?? const []).cast<String>().toSet();
    final model = _a['model'] as String?;
    return Dialog(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 640, maxHeight: 760),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            PanelHeader(
              title: widget.initial == null ? 'New agent' : 'Edit agent',
              icon: Icons.smart_toy_outlined,
              height: 52,
            ),
            Expanded(
              child: ListView(
                padding: const EdgeInsets.all(HkTokens.s5),
                children: [
                  Row(
                    children: [
                      PopupMenuButton<String>(
                        tooltip: 'Avatar',
                        onSelected: (v) => setState(() => _a['avatar'] = v),
                        itemBuilder: (_) => [
                          for (final e in _avatars)
                            PopupMenuItem(
                              value: e,
                              child: Text(
                                e,
                                style: const TextStyle(fontSize: 20),
                              ),
                            ),
                        ],
                        child: Container(
                          width: 52,
                          height: 52,
                          alignment: Alignment.center,
                          decoration: BoxDecoration(
                            color: hk.surface3,
                            borderRadius: BorderRadius.circular(14),
                          ),
                          child: Text(
                            _a['avatar'] as String? ?? '🤖',
                            style: const TextStyle(fontSize: 26),
                          ),
                        ),
                      ),
                      const SizedBox(width: HkTokens.s3),
                      Expanded(
                        child: TextField(
                          controller: _name,
                          decoration: const InputDecoration(
                            labelText: 'Name',
                            hintText: 'Code reviewer',
                          ),
                        ),
                      ),
                    ],
                  ),
                  const SizedBox(height: HkTokens.s3),
                  TextField(
                    controller: _desc,
                    decoration: const InputDecoration(
                      labelText: 'Description (shown on the card)',
                    ),
                  ),
                  const SectionLabel('Model & mode'),
                  DropdownButtonFormField<String?>(
                    initialValue: models.any((m) => m.reference == model)
                        ? model
                        : null,
                    isExpanded: true,
                    decoration: const InputDecoration(labelText: 'Model'),
                    items: [
                      const DropdownMenuItem<String?>(
                        value: null,
                        child: Text('Default model'),
                      ),
                      for (final m in models)
                        DropdownMenuItem<String?>(
                          value: m.reference,
                          child: Text(
                            '${m.name}  ·  ${m.provider}',
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                    ],
                    onChanged: (v) => setState(() => _a['model'] = v),
                  ),
                  const SizedBox(height: HkTokens.s3),
                  SegmentedButton<String>(
                    showSelectedIcon: false,
                    segments: const [
                      ButtonSegment(
                        value: 'build',
                        icon: Icon(Icons.build_outlined, size: 16),
                        label: Text('Build: may edit & run'),
                      ),
                      ButtonSegment(
                        value: 'plan',
                        icon: Icon(Icons.map_outlined, size: 16),
                        label: Text('Plan: read-only'),
                      ),
                    ],
                    selected: {(_a['mode'] as String?) ?? 'build'},
                    onSelectionChanged: (v) =>
                        setState(() => _a['mode'] = v.first),
                  ),
                  const SectionLabel('Persona'),
                  TextField(
                    controller: _system,
                    minLines: 4,
                    maxLines: 10,
                    decoration: const InputDecoration(
                      labelText: 'System prompt',
                      hintText:
                          'You review Rust code for correctness and clarity…',
                    ),
                  ),
                  const SectionLabel('Preferred tools'),
                  Wrap(
                    spacing: 6,
                    runSpacing: 6,
                    children: [
                      for (final t in _toolChoices)
                        FilterChip(
                          label: Text(t),
                          selected: tools.contains(t),
                          onSelected: (v) => setState(() {
                            final next = {...tools};
                            v ? next.add(t) : next.remove(t);
                            _a['tools'] = next.toList();
                          }),
                        ),
                    ],
                  ),
                  const SectionLabel('Sampling'),
                  SliderRow(
                    label: 'Temperature',
                    value: (_settings['temperature'] as num?)?.toDouble(),
                    fallback: 0.7,
                    min: 0,
                    max: 2,
                    divisions: 40,
                    onChanged: (v) => _setSetting(
                      'temperature',
                      v == null ? null : (v * 100).round() / 100,
                    ),
                  ),
                  SliderRow(
                    label: 'Max output tokens',
                    value: (_settings['max_output_tokens'] as num?)?.toDouble(),
                    fallback: 1024,
                    min: 64,
                    max: 32768,
                    divisions: 511,
                    onChanged: (v) =>
                        _setSetting('max_output_tokens', v?.round()),
                  ),
                  const SectionLabel('Conversation starters'),
                  TextField(
                    controller: _starters,
                    minLines: 2,
                    maxLines: 5,
                    decoration: const InputDecoration(
                      hintText: 'One per line, e.g. "Review my last commit"',
                    ),
                  ),
                ],
              ),
            ),
            Container(
              padding: const EdgeInsets.all(HkTokens.s3),
              decoration: BoxDecoration(
                border: Border(top: BorderSide(color: hk.border)),
              ),
              child: Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: [
                  TextButton(
                    onPressed: () => Navigator.pop(context),
                    child: const Text('Cancel'),
                  ),
                  const SizedBox(width: HkTokens.s2),
                  GradientButton(
                    label: 'Save agent',
                    icon: Icons.check,
                    onPressed: () {
                      if (_name.text.trim().isEmpty) return;
                      Navigator.pop(context, {
                        ..._a,
                        'name': _name.text.trim(),
                        'description': _desc.text.trim(),
                        'system_prompt': _system.text,
                        'starters': [
                          for (final l in _starters.text.split('\n'))
                            if (l.trim().isNotEmpty) l.trim(),
                        ],
                      });
                    },
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
