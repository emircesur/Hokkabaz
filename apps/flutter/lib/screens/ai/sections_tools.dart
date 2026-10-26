import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';

import '../../ffi/hk_gate.dart';
import '../../src/rust/api/chat.dart' as chat;
import '../../src/rust/api/settings.dart' as settings;
import '../../src/rust/dto.dart' show SessionInfoDto;
import '../../theme/tokens.dart';
import 'ai_common.dart';

/// Model arena (P9.11): same prompt to several models side by side.
class ArenaSection extends StatefulWidget {
  const ArenaSection({super.key});
  @override
  State<ArenaSection> createState() => _ArenaSectionState();
}

class _ArenaSectionState extends State<ArenaSection> {
  final _prompt = TextEditingController();
  final Set<String> _models = {};
  List<String> _all = const [];
  List<Map<String, dynamic>> _results = const [];
  bool _running = false;

  @override
  void initState() {
    super.initState();
    settings
        .listModels()
        .then((m) {
          if (mounted) {
            setState(() => _all = [for (final x in m.models) x.reference]);
          }
        })
        .catchError((_) {});
  }

  Future<void> _run() async {
    if (_prompt.text.trim().isEmpty || _models.isEmpty) return;
    setState(() => _running = true);
    final r = await guarded(
      context,
      () => hkList('arena.run', {
        'prompt': _prompt.text,
        'models': _models.toList(),
      }),
    );
    if (mounted) {
      setState(() {
        _running = false;
        if (r != null) _results = r;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final hk0 = context.hk;
    return SectionFrame(
      title: 'Model arena',
      subtitle: 'Send one prompt to up to 6 models at once; compare answers, latency and tokens.',
      actions: [
        FilledButton.icon(
          onPressed: _running ? null : _run,
          icon: const Icon(Icons.play_arrow),
          label: Text(_running ? 'Running…' : 'Compare'),
        ),
      ],
      child: Column(
        children: [
          TextField(
            controller: _prompt,
            minLines: 2,
            maxLines: 5,
            decoration: const InputDecoration(
              hintText: 'Prompt',
              border: OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: HkTokens.s2),
          SizedBox(
            height: 40,
            child: ListView(
              scrollDirection: Axis.horizontal,
              children: [
                for (final m in _all)
                  Padding(
                    padding: const EdgeInsets.only(right: 4),
                    child: FilterChip(
                      label: Text(m),
                      selected: _models.contains(m),
                      onSelected: (v) => setState(
                        () => v
                            ? (_models.length < 6 ? _models.add(m) : null)
                            : _models.remove(m),
                      ),
                    ),
                  ),
              ],
            ),
          ),
          const SizedBox(height: HkTokens.s2),
          Expanded(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                for (final r in _results)
                  Expanded(
                    child: Card(
                      child: Padding(
                        padding: const EdgeInsets.all(HkTokens.s3),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(
                              r['model'] as String? ?? '',
                              style: const TextStyle(
                                fontWeight: FontWeight.w600,
                              ),
                            ),
                            Text(
                              'TTFT ${r['ttft_ms'] ?? '-'} ms · total ${r['total_ms']} ms · out ${(r['usage'] as Map?)?['output_tokens'] ?? '?'} tok',
                              style: TextStyle(
                                fontSize: 12,
                                color: hk0.textMuted,
                              ),
                            ),
                            const Divider(),
                            Expanded(
                              child: SingleChildScrollView(
                                child: SelectableText(
                                  (r['error'] ?? r['text'] ?? '').toString(),
                                ),
                              ),
                            ),
                          ],
                        ),
                      ),
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

/// Usage & cost dashboard (P9.13).
class UsageSection extends StatefulWidget {
  const UsageSection({super.key});
  @override
  State<UsageSection> createState() => _UsageSectionState();
}

class _UsageSectionState extends State<UsageSection> {
  int _days = 30;
  late Future<Map<String, dynamic>> _data = hkMap('usage.summary', {
    'days': _days,
  });

  @override
  Widget build(BuildContext context) {
    final hk0 = context.hk;
    return SectionFrame(
      title: 'Usage & cost',
      subtitle: 'Tokens and estimated cost from your session logs (prices from the model catalog).',
      actions: [
        for (final d in const [7, 30, 90])
          Padding(
            padding: const EdgeInsets.only(left: 4),
            child: ChoiceChip(
              label: Text('${d}d'),
              selected: _days == d,
              onSelected: (_) => setState(() {
                _days = d;
                _data = hkMap('usage.summary', {'days': d});
              }),
            ),
          ),
      ],
      child: FutureBuilder<Map<String, dynamic>>(
        future: _data,
        builder: (context, snap) {
          if (!snap.hasData) {
            return const Center(child: CircularProgressIndicator());
          }
          final days = [
            for (final d in (snap.data!['days'] as List? ?? const []))
              (d as Map).cast<String, dynamic>(),
          ];
          final models = [
            for (final m in (snap.data!['models'] as List? ?? const []))
              (m as Map).cast<String, dynamic>(),
          ];
          final maxTok = days.fold<num>(
            1,
            (a, d) => (((d['input'] as num) + (d['output'] as num)) > a)
                ? (d['input'] as num) + (d['output'] as num)
                : a,
          );
          final cost = models.fold<double>(
            0,
            (a, m) => a + ((m['cost_usd'] as num?)?.toDouble() ?? 0),
          );
          return ListView(
            children: [
              Text(
                'Total cost: \$${cost.toStringAsFixed(4)}',
                style: Theme.of(context).textTheme.titleMedium,
              ),
              const SizedBox(height: HkTokens.s3),
              SizedBox(
                height: 160,
                child: Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    for (final d in days)
                      Expanded(
                        child: Tooltip(
                          message:
                              '${d['day']}: ${d['input']} in / ${d['output']} out · \$${((d['cost_usd'] as num?) ?? 0).toStringAsFixed(4)}',
                          child: Container(
                            margin: const EdgeInsets.symmetric(horizontal: 1),
                            height:
                                160 *
                                (((d['input'] as num) + (d['output'] as num)) /
                                        maxTok)
                                    .toDouble(),
                            color: hk0.accent,
                          ),
                        ),
                      ),
                  ],
                ),
              ),
              const SizedBox(height: HkTokens.s4),
              DataTable(
                columns: const [
                  DataColumn(label: Text('Model')),
                  DataColumn(label: Text('Input')),
                  DataColumn(label: Text('Output')),
                  DataColumn(label: Text('Cost')),
                ],
                rows: [
                  for (final m in models)
                    DataRow(
                      cells: [
                        DataCell(Text(m['model'].toString())),
                        DataCell(Text(m['input'].toString())),
                        DataCell(Text(m['output'].toString())),
                        DataCell(
                          Text(
                            '\$${((m['cost_usd'] as num?) ?? 0).toStringAsFixed(4)}',
                          ),
                        ),
                      ],
                    ),
                ],
              ),
            ],
          );
        },
      ),
    );
  }
}

/// Context inspector (P9.18) + pause control for a session.
class ContextSection extends StatefulWidget {
  const ContextSection({super.key});
  @override
  State<ContextSection> createState() => _ContextSectionState();
}

class _ContextSectionState extends State<ContextSection> {
  late Future<List<SessionInfoDto>> _sessions = chat.listSessions(
    limit: 50,
    offset: 0,
  );
  String? _sel;
  Map<String, dynamic>? _ctx;

  Future<void> _open(String id) async {
    setState(() => _sel = id);
    final r = await guarded(
      context,
      () => hkMap('session.context', {'session_id': id}),
    );
    if (mounted) setState(() => _ctx = r);
  }

  @override
  Widget build(BuildContext context) {
    final hk0 = context.hk;
    final rows = [
      for (final m in (_ctx?['messages'] as List? ?? const []))
        (m as Map).cast<String, dynamic>(),
    ];
    final total = (_ctx?['total_tokens'] as num?) ?? 1;
    return SectionFrame(
      title: 'Context inspector',
      subtitle: 'What the next model request carries: every message with its token estimate. Pause a running turn after its current step.',
      actions: [
        IconButton(
          tooltip: 'Refresh',
          icon: const Icon(Icons.refresh),
          onPressed: () => setState(
            () => _sessions = chat.listSessions(limit: 50, offset: 0),
          ),
        ),
        if (_sel != null)
          OutlinedButton.icon(
            onPressed: () => guarded(
              context,
              () => hk('session.pause', {'session_id': _sel}),
              ok: 'Will pause after the current step',
            ),
            icon: const Icon(Icons.pause),
            label: const Text('Pause after step'),
          ),
      ],
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 260,
            child: FutureBuilder<List<SessionInfoDto>>(
              future: _sessions,
              builder: (context, snap) => ListView(
                children: [
                  for (final s in snap.data ?? const <SessionInfoDto>[])
                    ListTile(
                      dense: true,
                      selected: s.id == _sel,
                      leading: Icon(
                        s.running
                            ? Icons.play_circle
                            : Icons.chat_bubble_outline,
                        size: 18,
                      ),
                      title: Text(
                        s.title ?? s.id,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                      ),
                      subtitle: Text(s.model ?? '', maxLines: 1),
                      onTap: () => _open(s.id),
                    ),
                ],
              ),
            ),
          ),
          const VerticalDivider(),
          Expanded(
            child: _ctx == null
                ? Center(
                    child: Text(
                      'Pick a session',
                      style: TextStyle(color: hk0.textMuted),
                    ),
                  )
                : ListView(
                    children: [
                      Text(
                        '~$total tokens in ${rows.length} messages',
                        style: Theme.of(context).textTheme.titleMedium,
                      ),
                      const SizedBox(height: HkTokens.s2),
                      for (final r in rows)
                        Padding(
                          padding: const EdgeInsets.only(bottom: 6),
                          child: Row(
                            crossAxisAlignment: CrossAxisAlignment.start,
                            children: [
                              SizedBox(
                                width: 80,
                                child: Text(
                                  r['role'].toString(),
                                  style: TextStyle(color: hk0.accent2),
                                ),
                              ),
                              SizedBox(
                                width: 120,
                                child: Column(
                                  crossAxisAlignment: CrossAxisAlignment.start,
                                  children: [
                                    Text(
                                      '${r['tokens']} tok',
                                      style: const TextStyle(fontSize: 12),
                                    ),
                                    LinearProgressIndicator(
                                      value: ((r['tokens'] as num) / total)
                                          .toDouble()
                                          .clamp(0, 1),
                                    ),
                                  ],
                                ),
                              ),
                              const SizedBox(width: HkTokens.s2),
                              Expanded(
                                child: Text(
                                  r['preview'].toString(),
                                  maxLines: 2,
                                  overflow: TextOverflow.ellipsis,
                                  style: TextStyle(color: hk0.textMuted),
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

/// Checkpoints & rewind (P9.19).
class CheckpointsSection extends StatefulWidget {
  const CheckpointsSection({super.key});
  @override
  State<CheckpointsSection> createState() => _CheckpointsSectionState();
}

class _CheckpointsSectionState extends State<CheckpointsSection> {
  String? _dir;
  Future<List<Map<String, dynamic>>>? _list;

  void _reload() =>
      setState(() => _list = hkList('checkpoints.list', {'path': _dir}));

  @override
  Widget build(BuildContext context) => SectionFrame(
    title: 'Checkpoints',
    subtitle: 'Snapshots of a working folder in a shadow repo (your own .git is untouched). Agents create them with the checkpoint tool.',
    actions: [
      OutlinedButton.icon(
        icon: const Icon(Icons.folder_open),
        label: Text(
          _dir == null ? 'Choose folder' : _dir!.split(RegExp(r'[\\/]')).last,
        ),
        onPressed: () async {
          final d = await FilePicker.getDirectoryPath(
            dialogTitle: 'Working folder',
          );
          if (d != null) {
            _dir = d;
            _reload();
          }
        },
      ),
      const SizedBox(width: 8),
      FilledButton.icon(
        onPressed: _dir == null
            ? null
            : () async {
                await guarded(
                  context,
                  () => hk('checkpoints.create', {
                    'path': _dir,
                    'label': 'manual',
                  }),
                  ok: 'Checkpoint created',
                );
                _reload();
              },
        icon: const Icon(Icons.save_outlined),
        label: const Text('Snapshot now'),
      ),
    ],
    child: _list == null
        ? const Center(child: Text('Choose a folder'))
        : FutureBuilder<List<Map<String, dynamic>>>(
            future: _list,
            builder: (context, snap) => ListView(
              children: [
                for (final c in snap.data ?? const <Map<String, dynamic>>[])
                  ItemTile(
                    leading: Icons.history,
                    title: c['label'] as String? ?? '',
                    subtitle:
                        '${c['ts']} · ${(c['id'] as String? ?? '').padRight(8).substring(0, 8)}',
                    trailing: [
                      TextButton(
                        onPressed: () async {
                          await guarded(
                            context,
                            () => hk('checkpoints.restore', {
                              'path': _dir,
                              'id': c['id'],
                            }),
                            ok: 'Restored',
                          );
                          _reload();
                        },
                        child: const Text('Restore'),
                      ),
                    ],
                  ),
              ],
            ),
          ),
  );
}

/// Speech + image generation playground (P9.14, P9.16).
class GenerateSection extends StatefulWidget {
  const GenerateSection({super.key});
  @override
  State<GenerateSection> createState() => _GenerateSectionState();
}

class _GenerateSectionState extends State<GenerateSection> {
  final _text = TextEditingController();
  final _prompt = TextEditingController();
  String? _audio;
  String? _image;
  bool _busy = false;

  @override
  Widget build(BuildContext context) => SectionFrame(
    title: 'Generate',
    subtitle: 'Text-to-speech (OpenAI-compatible or Piper) and images (OpenAI-compatible or ComfyUI). Configure engines in AI settings → Generation.',
    child: ListView(
      children: [
        Text('Speech', style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: HkTokens.s2),
        TextField(
          controller: _text,
          minLines: 2,
          maxLines: 4,
          decoration: const InputDecoration(
            hintText: 'Text to speak',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: HkTokens.s2),
        Row(
          children: [
            FilledButton.icon(
              onPressed: _busy
                  ? null
                  : () async {
                      setState(() => _busy = true);
                      final p = await guarded(
                        context,
                        () => hk('tts.speak', {'text': _text.text}),
                      );
                      if (mounted) {
                        setState(() {
                          _busy = false;
                          _audio = p as String?;
                        });
                      }
                    },
              icon: const Icon(Icons.record_voice_over_outlined),
              label: const Text('Speak'),
            ),
            const SizedBox(width: HkTokens.s2),
            if (_audio != null) Expanded(child: SelectableText(_audio!)),
          ],
        ),
        const SizedBox(height: HkTokens.s6),
        Text('Image', style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: HkTokens.s2),
        TextField(
          controller: _prompt,
          minLines: 2,
          maxLines: 4,
          decoration: const InputDecoration(
            hintText: 'Image prompt',
            border: OutlineInputBorder(),
          ),
        ),
        const SizedBox(height: HkTokens.s2),
        Row(
          children: [
            FilledButton.icon(
              onPressed: _busy
                  ? null
                  : () async {
                      setState(() => _busy = true);
                      final p = await guarded(
                        context,
                        () => hk('image.generate', {'prompt': _prompt.text}),
                      );
                      if (mounted) {
                        setState(() {
                          _busy = false;
                          _image = p as String?;
                        });
                      }
                    },
              icon: const Icon(Icons.image_outlined),
              label: const Text('Generate'),
            ),
            if (_busy)
              const Padding(
                padding: EdgeInsets.all(8),
                child: SizedBox(
                  width: 18,
                  height: 18,
                  child: CircularProgressIndicator(strokeWidth: 2),
                ),
              ),
          ],
        ),
        if (_image != null)
          Padding(
            padding: const EdgeInsets.only(top: HkTokens.s3),
            child: Align(
              alignment: Alignment.centerLeft,
              child: Image.file(File(_image!), height: 320),
            ),
          ),
      ],
    ),
  );
}
