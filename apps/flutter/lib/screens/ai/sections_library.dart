import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../ffi/hk_gate.dart';
import '../../theme/tokens.dart';
import '../flows/flow_store.dart';
import '../flows/model/workflow_model.dart';
import '../chat/chat_providers.dart' show selectedSessionProvider;
import 'ai_common.dart';

/// Template gallery (P9.42): starter flows, agents, prompts, presets; install with inputs.
class TemplatesSection extends StatefulWidget {
  const TemplatesSection({super.key});
  @override
  State<TemplatesSection> createState() => _TemplatesSectionState();
}

class _TemplatesSectionState extends State<TemplatesSection> {
  late Future<List<Map<String, dynamic>>> _items = hkList('templates.list');
  String _kind = 'all';
  String _q = '';

  static const _kinds = ['all', 'flow', 'agent', 'prompt', 'settings_preset'];

  IconData _icon(String k) => switch (k) {
    'flow' => Icons.account_tree_outlined,
    'agent' => Icons.smart_toy_outlined,
    'prompt' => Icons.notes,
    'settings_preset' => Icons.tune,
    _ => Icons.extension_outlined,
  };

  Future<void> _install(Map<String, dynamic> t) async {
    final inputs = [
      for (final i in (t['inputs'] as List? ?? const []))
        (i as Map).cast<String, dynamic>(),
    ];
    Map<String, dynamic> values = {};
    if (inputs.isNotEmpty) {
      final r = await editItem(
        context,
        title: 'Install "${t['name']}"',
        fields: [
          for (final i in inputs)
            FieldSpec(
              i['key'] as String,
              '${i['label']}${i['required'] == true ? ' *' : ''}',
            ),
        ],
        initial: {
          for (final i in inputs)
            if (i['default'] != null) i['key'] as String: i['default'],
        },
      );
      if (r == null) return;
      values = {
        for (final e in r.entries)
          if (e.value != null && e.value.toString().isNotEmpty) e.key: e.value,
      };
    }
    if (!mounted) return;
    final out = await guarded(
      context,
      () => hkMap('templates.install', {'id': t['id'], 'inputs': values}),
    );
    if (out == null || !mounted) return;
    if (out['kind'] == 'flow') {
      final store = await FlowStore.open();
      await store.create(
        t['name'] as String,
        from: FlowWorkflow.parse(prettyJson(out['payload'])),
      );
    }
    if (mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text('Installed ${t['name']} (${out['kind']})')),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return SectionFrame(
      title: 'Templates',
      subtitle: 'Starter flows (n8n-compatible), agents, prompts and AI presets. Install fills the inputs and adds them to your app.',
      actions: [
        IconButton(
          tooltip: 'Include GitHub registry',
          icon: const Icon(Icons.cloud_download_outlined),
          onPressed: () => setState(
            () => _items = hkList('templates.list', {'remote': true}),
          ),
        ),
      ],
      child: Column(
        children: [
          Row(
            children: [
              Expanded(
                child: TextField(
                  decoration: const InputDecoration(
                    prefixIcon: Icon(Icons.search),
                    hintText: 'Search templates',
                    isDense: true,
                    border: OutlineInputBorder(),
                  ),
                  onChanged: (v) => setState(() => _q = v.toLowerCase()),
                ),
              ),
              const SizedBox(width: HkTokens.s2),
              for (final k in _kinds)
                Padding(
                  padding: const EdgeInsets.only(left: 4),
                  child: ChoiceChip(
                    label: Text(k == 'settings_preset' ? 'presets' : k),
                    selected: _kind == k,
                    onSelected: (_) => setState(() => _kind = k),
                  ),
                ),
            ],
          ),
          const SizedBox(height: HkTokens.s3),
          Expanded(
            child: FutureBuilder<List<Map<String, dynamic>>>(
              future: _items,
              builder: (context, snap) {
                if (!snap.hasData) {
                  return const Center(child: CircularProgressIndicator());
                }
                final list = snap.data!
                    .where((t) => _kind == 'all' || t['kind'] == _kind)
                    .where(
                      (t) =>
                          _q.isEmpty ||
                          '${t['name']} ${t['description']}'
                              .toLowerCase()
                              .contains(_q),
                    )
                    .toList();
                return GridView.builder(
                  gridDelegate: const SliverGridDelegateWithMaxCrossAxisExtent(
                    maxCrossAxisExtent: 300,
                    mainAxisExtent: 170,
                    crossAxisSpacing: 12,
                    mainAxisSpacing: 12,
                  ),
                  itemCount: list.length,
                  itemBuilder: (context, i) {
                    final t = list[i];
                    return Card(
                      child: Padding(
                        padding: const EdgeInsets.all(HkTokens.s3),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Row(
                              children: [
                                Icon(
                                  _icon(t['kind'] as String? ?? ''),
                                  color: hk.accent,
                                ),
                                const SizedBox(width: HkTokens.s2),
                                Expanded(
                                  child: Text(
                                    t['name'] as String? ?? '',
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: const TextStyle(
                                      fontWeight: FontWeight.w600,
                                    ),
                                  ),
                                ),
                              ],
                            ),
                            const SizedBox(height: HkTokens.s2),
                            Expanded(
                              child: Text(
                                t['description'] as String? ?? '',
                                maxLines: 3,
                                overflow: TextOverflow.ellipsis,
                                style: TextStyle(color: hk.textMuted),
                              ),
                            ),
                            Row(
                              children: [
                                Text(
                                  t['kind'] as String? ?? '',
                                  style: TextStyle(
                                    fontSize: 12,
                                    color: hk.textMuted,
                                  ),
                                ),
                                const Spacer(),
                                if (t['remote'] == true)
                                  const Text(
                                    'registry',
                                    style: TextStyle(fontSize: 12),
                                  )
                                else
                                  FilledButton.tonal(
                                    onPressed: () => _install(t),
                                    child: const Text('Install'),
                                  ),
                              ],
                            ),
                          ],
                        ),
                      ),
                    );
                  },
                );
              },
            ),
          ),
        ],
      ),
    );
  }
}

/// Generic CRUD over a list document (`<domain>.list|save|delete`).
class DocListSection extends StatefulWidget {
  const DocListSection({
    super.key,
    required this.domain,
    required this.title,
    required this.subtitle,
    required this.fields,
    required this.icon,
    this.idPrefix = 'item',
    this.describe,
    this.extraActions,
  });

  final String domain;
  final String title;
  final String subtitle;
  final List<FieldSpec> fields;
  final IconData icon;
  final String idPrefix;
  final String Function(Map<String, dynamic>)? describe;
  final List<Widget> Function(BuildContext, Map<String, dynamic>)? extraActions;

  @override
  State<DocListSection> createState() => _DocListSectionState();
}

class _DocListSectionState extends State<DocListSection> {
  late Future<List<Map<String, dynamic>>> _items = hkList(
    '${widget.domain}.list',
  );

  void _reload() => setState(() => _items = hkList('${widget.domain}.list'));

  Future<void> _edit([Map<String, dynamic>? item]) async {
    final r = await editItem(
      context,
      title: item == null ? 'New' : 'Edit',
      fields: widget.fields,
      initial: item ?? {'id': newId(widget.idPrefix)},
    );
    if (r == null || !mounted) return;
    await guarded(context, () => hk('${widget.domain}.save', {'item': r}));
    _reload();
  }

  @override
  Widget build(BuildContext context) => SectionFrame(
    title: widget.title,
    subtitle: widget.subtitle,
    actions: [
      FilledButton.icon(
        onPressed: _edit,
        icon: const Icon(Icons.add),
        label: const Text('New'),
      ),
    ],
    child: FutureBuilder<List<Map<String, dynamic>>>(
      future: _items,
      builder: (context, snap) {
        if (!snap.hasData) {
          return const Center(child: CircularProgressIndicator());
        }
        final list = snap.data!;
        if (list.isEmpty) {
          return Center(
            child: Text(
              'Nothing yet',
              style: TextStyle(color: context.hk.textMuted),
            ),
          );
        }
        return ListView(
          children: [
            for (final it in list)
              ItemTile(
                leading: widget.icon,
                title: (it['name'] ?? it['id'] ?? '').toString(),
                subtitle: widget.describe?.call(it),
                onTap: () => _edit(it),
                trailing: [
                  ...?widget.extraActions?.call(context, it),
                  IconButton(
                    tooltip: 'Delete',
                    icon: const Icon(Icons.delete_outline),
                    onPressed: () async {
                      await guarded(
                        context,
                        () => hk('${widget.domain}.delete', {'id': it['id']}),
                      );
                      _reload();
                    },
                  ),
                ],
              ),
          ],
        );
      },
    ),
  );
}

/// Custom agent builder (P9.21).
class AgentsSection extends StatelessWidget {
  const AgentsSection({super.key});
  @override
  Widget build(BuildContext context) => DocListSection(
    domain: 'agents',
    title: 'Agents',
    subtitle: 'Compose agents: system prompt, model, tools and mode. Use them from chat or the Agent node.',
    icon: Icons.smart_toy_outlined,
    idPrefix: 'agent',
    fields: const [
      FieldSpec('name', 'Name'),
      FieldSpec('model', 'Model', hint: 'deepseek/deepseek-chat'),
      FieldSpec('mode', 'Mode', hint: 'build | plan'),
      FieldSpec(
        'tools',
        'Tools (comma separated)',
        list: true,
        hint: 'read, edit, bash, memory',
      ),
      FieldSpec('system_prompt', 'System prompt', multiline: true),
    ],
    describe: (a) =>
        '${a['model'] ?? ''} · ${(a['tools'] as List?)?.join(', ') ?? ''}',
    extraActions: (context, a) => [
      Consumer(
        builder: (ctx, ref, _) => IconButton(
          tooltip: 'Start a chat as this agent',
          icon: const Icon(Icons.chat_outlined),
          onPressed: () async {
            final info = await guarded(
              ctx,
              () => hkMap('agents.start', {'id': a['id']}),
            );
            if (info == null || !ctx.mounted) return;
            ref
                .read(selectedSessionProvider.notifier)
                .select(info['id'] as String?);
            ctx.go('/chat');
          },
        ),
      ),
    ],
  );
}

/// Prompt library (P9.26).
class PromptsSection extends StatelessWidget {
  const PromptsSection({super.key});
  @override
  Widget build(BuildContext context) => DocListSection(
    domain: 'prompts',
    title: 'Prompt library',
    subtitle: 'Reusable prompts with {{variables}}. Copy into chat or flows.',
    icon: Icons.notes,
    idPrefix: 'prompt',
    fields: const [
      FieldSpec('name', 'Name'),
      FieldSpec('text', 'Prompt', multiline: true),
    ],
    describe: (p) => (p['text'] ?? '').toString(),
  );
}

/// Scheduled agents (P9.20) + publish calendar (P9.39).
class SchedulesSection extends StatelessWidget {
  const SchedulesSection({super.key});
  @override
  Widget build(BuildContext context) => DefaultTabController(
    length: 2,
    child: Column(
      children: [
        const TabBar(
          tabs: [
            Tab(text: 'Scheduled agents'),
            Tab(text: 'Publish calendar'),
          ],
        ),
        Expanded(
          child: TabBarView(
            children: [
              DocListSection(
                domain: 'schedules',
                title: 'Scheduled agents',
                subtitle: 'Run an agent prompt every N minutes; results appear as new chat sessions.',
                icon: Icons.alarm,
                idPrefix: 'sched',
                fields: const [
                  FieldSpec('name', 'Name'),
                  FieldSpec('prompt', 'Prompt', multiline: true),
                  FieldSpec('model', 'Model (optional)'),
                  FieldSpec('every_minutes', 'Every (minutes)', number: true),
                ],
                describe: (s) =>
                    'every ${s['every_minutes']} min · ${s['prompt']}',
              ),
              DocListSection(
                domain: 'posts',
                title: 'Publish calendar',
                subtitle:
                    'Upload a rendered Studio clip to YouTube at a set time.',
                icon: Icons.event,
                idPrefix: 'post',
                fields: const [
                  FieldSpec('name', 'Label'),
                  FieldSpec('project', 'Studio project id'),
                  FieldSpec('clip', 'Clip id'),
                  FieldSpec(
                    'privacy',
                    'Privacy',
                    hint: 'private | unlisted | public',
                  ),
                  FieldSpec('at_ms', 'When (unix ms)', number: true),
                ],
                describe: (p) =>
                    '${p['done'] == true ? 'posted' : 'scheduled'} · ${DateTime.fromMillisecondsSinceEpoch((p['at_ms'] as num?)?.toInt() ?? 0)}',
              ),
            ],
          ),
        ),
      ],
    ),
  );
}

/// Long-term memory (P9.9).
class MemorySection extends StatefulWidget {
  const MemorySection({super.key});
  @override
  State<MemorySection> createState() => _MemorySectionState();
}

class _MemorySectionState extends State<MemorySection> {
  late Future<List<Map<String, dynamic>>> _items = hkList('memory.list');
  final _q = TextEditingController();

  void _reload() => setState(
    () => _items = _q.text.trim().isEmpty
        ? hkList('memory.list')
        : hkList('memory.search', {'query': _q.text}),
  );

  @override
  Widget build(BuildContext context) => SectionFrame(
    title: 'Memory',
    subtitle: 'Facts the agent remembers across sessions (tool: memory). Edit or remove anything here.',
    actions: [
      FilledButton.icon(
        icon: const Icon(Icons.add),
        label: const Text('Remember'),
        onPressed: () async {
          final r = await editItem(
            context,
            title: 'New memory',
            fields: const [
              FieldSpec('text', 'Fact', multiline: true),
              FieldSpec('project', 'Project folder (empty = everywhere)'),
            ],
          );
          if (r == null || !context.mounted) return;
          final p = (r['project'] as String?)?.trim();
          await guarded(
            context,
            () => hk('memory.save', {
              'text': r['text'],
              if (p != null && p.isNotEmpty) 'project': p,
            }),
          );
          _reload();
        },
      ),
    ],
    child: Column(
      children: [
        TextField(
          controller: _q,
          decoration: const InputDecoration(
            prefixIcon: Icon(Icons.search),
            hintText: 'Search memories',
            isDense: true,
            border: OutlineInputBorder(),
          ),
          onSubmitted: (_) => _reload(),
        ),
        const SizedBox(height: HkTokens.s2),
        Expanded(
          child: FutureBuilder<List<Map<String, dynamic>>>(
            future: _items,
            builder: (context, snap) {
              if (!snap.hasData) {
                return const Center(child: CircularProgressIndicator());
              }
              return ListView(
                children: [
                  for (final m in snap.data!)
                    ItemTile(
                      leading: Icons.psychology_outlined,
                      title: m['text'] as String? ?? '',
                      subtitle: m['scope'] as String?,
                      trailing: [
                        IconButton(
                          icon: const Icon(Icons.delete_outline),
                          onPressed: () async {
                            await guarded(
                              context,
                              () => hk('memory.delete', {'id': m['id']}),
                            );
                            _reload();
                          },
                        ),
                      ],
                    ),
                ],
              );
            },
          ),
        ),
      ],
    ),
  );
}

/// Local knowledge bases / RAG (P9.10).
class KnowledgeSection extends StatefulWidget {
  const KnowledgeSection({super.key});
  @override
  State<KnowledgeSection> createState() => _KnowledgeSectionState();
}

class _KnowledgeSectionState extends State<KnowledgeSection> {
  late Future<List<Map<String, dynamic>>> _bases = hkList('knowledge.list');
  List<Map<String, dynamic>> _hits = const [];
  bool _indexing = false;

  Future<void> _add() async {
    final dir = await FilePicker.getDirectoryPath(
      dialogTitle: 'Folder to index',
    );
    if (dir == null || !mounted) return;
    final r = await editItem(
      context,
      title: 'Knowledge base name',
      fields: const [FieldSpec('name', 'Name')],
      initial: {'name': dir.split(RegExp(r'[\\/]')).last},
    );
    if (r == null || !mounted) return;
    setState(() => _indexing = true);
    final out = await guarded(
      context,
      () => hkMap('knowledge.index', {'name': r['name'], 'path': dir}),
    );
    if (!mounted) return;
    setState(() {
      _indexing = false;
      _bases = hkList('knowledge.list');
    });
    if (out != null) {
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: Text(
            'Indexed ${out['files']} files, ${out['chunks']} chunks',
          ),
        ),
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final hk0 = context.hk;
    return SectionFrame(
      title: 'Knowledge',
      subtitle: 'Index folders (docs, notes, code). Agents search them with the knowledge tool and cite file:line.',
      actions: [
        if (_indexing)
          const Padding(
            padding: EdgeInsets.all(8),
            child: SizedBox(
              width: 18,
              height: 18,
              child: CircularProgressIndicator(strokeWidth: 2),
            ),
          ),
        FilledButton.icon(
          onPressed: _indexing ? null : _add,
          icon: const Icon(Icons.create_new_folder_outlined),
          label: const Text('Index folder'),
        ),
      ],
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 280,
            child: FutureBuilder<List<Map<String, dynamic>>>(
              future: _bases,
              builder: (context, snap) => ListView(
                children: [
                  for (final b in snap.data ?? const <Map<String, dynamic>>[])
                    ItemTile(
                      leading: Icons.folder_outlined,
                      title: b['name'] as String? ?? '',
                      subtitle: '${b['chunks']} chunks · ${b['root']}',
                      trailing: [
                        IconButton(
                          icon: const Icon(Icons.delete_outline),
                          onPressed: () async {
                            await guarded(
                              context,
                              () => hk('knowledge.remove', {'name': b['name']}),
                            );
                            setState(() => _bases = hkList('knowledge.list'));
                          },
                        ),
                      ],
                    ),
                ],
              ),
            ),
          ),
          const SizedBox(width: HkTokens.s3),
          Expanded(
            child: Column(
              children: [
                TextField(
                  decoration: const InputDecoration(
                    prefixIcon: Icon(Icons.search),
                    hintText: 'Ask your knowledge bases',
                    isDense: true,
                    border: OutlineInputBorder(),
                  ),
                  onSubmitted: (q) async {
                    final r = await guarded(
                      context,
                      () => hkList('knowledge.search', {'query': q}),
                    );
                    if (r != null) setState(() => _hits = r);
                  },
                ),
                const SizedBox(height: HkTokens.s2),
                Expanded(
                  child: ListView(
                    children: [
                      for (final h in _hits)
                        Card(
                          child: Padding(
                            padding: const EdgeInsets.all(HkTokens.s3),
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                Text(
                                  '${h['base']} · ${h['cite']}',
                                  style: TextStyle(
                                    color: hk0.accent,
                                    fontWeight: FontWeight.w600,
                                  ),
                                ),
                                const SizedBox(height: 4),
                                Text(
                                  h['text'] as String? ?? '',
                                  maxLines: 8,
                                  overflow: TextOverflow.ellipsis,
                                  style: const TextStyle(
                                    fontFamily: HkTokens.fontMono,
                                    fontSize: 12,
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
