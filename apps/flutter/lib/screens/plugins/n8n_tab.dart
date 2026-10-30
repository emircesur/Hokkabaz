import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/api/n8n.dart' as api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import '../flows/flow_store.dart';
import '../flows/model/workflow_model.dart';
import 'credential_dialogs.dart';
import 'plugins_providers.dart';

class N8nLink {
  const N8nLink(this.credentialId, this.delegate);
  final String? credentialId;
  final bool delegate;
}

class N8nLinkNotifier extends AsyncNotifier<N8nLink> {
  @override
  Future<N8nLink> build() async {
    final j = jsonDecode(await api.n8NStatus()) as Map;
    return N8nLink(j['credential_id'] as String?, j['delegate'] == true);
  }

  Future<String?> save(String? credentialId, bool delegate) async {
    try {
      await api.n8NConfigure(credentialId: credentialId, delegate: delegate);
      state = AsyncData(
        N8nLink(credentialId, delegate && credentialId != null),
      );
      ref.invalidate(n8nWorkflowsProvider);
      return null;
    } catch (e) {
      return describeError(e);
    }
  }
}

final n8nLinkProvider = AsyncNotifierProvider<N8nLinkNotifier, N8nLink>(
  N8nLinkNotifier.new,
);

class RemoteWorkflow {
  RemoteWorkflow(Map<String, dynamic> j)
    : id = '${j['id']}',
      name = '${j['name']}',
      active = j['active'] == true,
      nodes = (j['nodes'] as num?)?.toInt();
  final String id, name;
  final bool active;
  final int? nodes;
}

final n8nWorkflowsProvider = FutureProvider.autoDispose<List<RemoteWorkflow>>((
  ref,
) async {
  final link = await ref.watch(n8nLinkProvider.future);
  if (link.credentialId == null) return const [];
  final raw = jsonDecode(await api.n8NListWorkflows()) as List;
  return raw.map((e) => RemoteWorkflow((e as Map).cast())).toList();
});

/// Link to the user's own n8n: pull workflows into local flows, run unsupported nodes there.
class N8nTab extends ConsumerStatefulWidget {
  const N8nTab({super.key});

  @override
  ConsumerState<N8nTab> createState() => _N8nTabState();
}

class _N8nTabState extends ConsumerState<N8nTab> {
  bool _busy = false;

  void _toast(String m) => ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(m)));

  Future<void> _test() async {
    setState(() => _busy = true);
    try {
      await api.n8NTest();
      _toast('Connected');
    } catch (e) {
      _toast(describeError(e));
    }
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _import(RemoteWorkflow w) async {
    setState(() => _busy = true);
    try {
      final r = jsonDecode(await api.n8NPull(id: w.id)) as Map;
      final wf = FlowWorkflow.fromJson(r['workflow']);
      final store = await FlowStore.open();
      await store.create(
        w.name.isEmpty ? 'Imported from n8n' : w.name,
        from: wf,
      );
      final un = (r['unmapped'] as List).cast<Map>();
      _toast(
        un.isEmpty
            ? 'Imported "${w.name}" as a local flow'
            : 'Imported "${w.name}". ${un.length} credential(s) need to be picked: ${un.map((u) => u['name']).toSet().join(', ')}',
      );
    } catch (e) {
      _toast('Import failed: ${describeError(e)}');
    }
    if (mounted) setState(() => _busy = false);
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final link = ref.watch(n8nLinkProvider);
    final creds = (ref.watch(credentialBookProvider).value ?? const [])
        .where((c) => c.type == 'n8nApi')
        .toList();
    final remote = ref.watch(n8nWorkflowsProvider);
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Text(
          'Connect your own n8n instance to import its workflows and to run node types '
          'Hokkabaz does not have yet. Requires an n8n API key (Settings > n8n API on your instance).',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s3),
        link.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (l) {
            final known = creds.any((c) => c.id == l.credentialId);
            return Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Row(
                  crossAxisAlignment: CrossAxisAlignment.end,
                  children: [
                    Expanded(
                      child: DropdownButtonFormField<String?>(
                        key: ValueKey('n8n-${l.credentialId}-${creds.length}'),
                        initialValue: known ? l.credentialId : null,
                        isExpanded: true,
                        decoration: const InputDecoration(
                          labelText: 'n8n API credential',
                        ),
                        items: [
                          const DropdownMenuItem<String?>(
                            value: null,
                            child: Text('Not linked'),
                          ),
                          for (final c in creds)
                            DropdownMenuItem<String?>(
                              value: c.id,
                              child: Text(c.name),
                            ),
                        ],
                        onChanged: (v) async {
                          final err = await ref
                              .read(n8nLinkProvider.notifier)
                              .save(v, l.delegate);
                          if (err != null) _toast(err);
                        },
                      ),
                    ),
                    IconButton(
                      tooltip: 'New n8n credential',
                      icon: const Icon(Icons.add),
                      onPressed: () async {
                        final made = await showCredentialDialog(
                          context,
                          fixedType: 'n8nApi',
                        );
                        if (made != null) {
                          final err = await ref
                              .read(n8nLinkProvider.notifier)
                              .save(made.id, l.delegate);
                          if (err != null) _toast(err);
                        }
                      },
                    ),
                  ],
                ),
                const SizedBox(height: HkTokens.s2),
                Row(
                  children: [
                    const Expanded(
                      child: Text('Run unsupported nodes on this instance'),
                    ),
                    Switch(
                      value: l.delegate,
                      onChanged: l.credentialId == null
                          ? null
                          : (v) async {
                              final err = await ref
                                  .read(n8nLinkProvider.notifier)
                                  .save(l.credentialId, v);
                              if (err != null) _toast(err);
                            },
                    ),
                  ],
                ),
                if (l.delegate)
                  Text(
                    'Each unsupported node briefly creates a hidden workflow on your instance. '
                    'Items leave this device. Only the first input and output run, binary data is not sent.',
                    style: text.bodySmall?.copyWith(color: hk.warn),
                  ),
                const SizedBox(height: HkTokens.s2),
                OutlinedButton.icon(
                  icon: const Icon(Icons.wifi_tethering, size: 18),
                  label: const Text('Test connection'),
                  onPressed: _busy || l.credentialId == null ? null : _test,
                ),
              ],
            );
          },
        ),
        const SizedBox(height: HkTokens.s4),
        Row(
          children: [
            Expanded(
              child: Text('Workflows on the instance', style: text.titleSmall),
            ),
            IconButton(
              tooltip: 'Refresh',
              icon: const Icon(Icons.refresh),
              onPressed: () => ref.invalidate(n8nWorkflowsProvider),
            ),
          ],
        ),
        remote.when(
          loading: () => const Padding(
            padding: EdgeInsets.all(HkTokens.s4),
            child: Center(child: CircularProgressIndicator()),
          ),
          error: (e, _) =>
              Text(describeError(e), style: TextStyle(color: hk.err)),
          data: (list) => list.isEmpty
              ? Text(
                  'Nothing to show. Link a credential to list workflows.',
                  style: TextStyle(color: hk.textMuted),
                )
              : Column(
                  children: [
                    for (final w in list)
                      Card(
                        margin: const EdgeInsets.only(bottom: HkTokens.s2),
                        child: ListTile(
                          leading: Icon(
                            w.active ? Icons.bolt : Icons.account_tree_outlined,
                            color: w.active ? hk.ok : null,
                          ),
                          title: Text(w.name),
                          subtitle: Text(
                            '${w.nodes ?? '?'} nodes${w.active ? ' · active' : ''}',
                          ),
                          trailing: FilledButton(
                            onPressed: _busy ? null : () => _import(w),
                            child: const Text('Import'),
                          ),
                        ),
                      ),
                  ],
                ),
        ),
      ],
    );
  }
}
