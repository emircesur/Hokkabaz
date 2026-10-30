import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'model/flow_editor.dart';
import 'model/n8n_clipboard.dart';
import 'model/node_types.dart';
import 'model/run_overlay.dart';
import 'model/workflow_model.dart';
import 'node_card.dart';
import 'node_io_tab.dart';
import 'params_form.dart';
import '../plugins/credential_dialogs.dart';
import '../plugins/plugins_providers.dart';

/// Right-hand inspector for one node: rename, generated params form, node
/// settings and the raw n8n JSON (DESIGN §4.4 "node panel").
class NodePanel extends StatelessWidget {
  const NodePanel({
    super.key,
    required this.editor,
    required this.nodeName,
    required this.types,
    required this.onClose,
    this.overlay,
  });

  final FlowEditor editor;
  final String nodeName;
  final Map<String, NodeTypeInfo> types;
  final VoidCallback onClose;

  /// Run shown on the canvas; feeds the Input / Output tabs.
  final RunOverlay? overlay;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: editor,
      builder: (context, _) {
        final node = editor.workflow.node(nodeName);
        if (node == null) return const SizedBox.shrink();
        final hk = context.hk;
        final text = Theme.of(context).textTheme;
        final info = types[node.type];
        final cat = categoryOf(node.type, group: info?.group ?? const []);
        final unsupported =
            info == null || !info.supportsVersion(node.typeVersion);
        return DefaultTabController(
          length: 5,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(
                  HkTokens.s3,
                  HkTokens.s3,
                  HkTokens.s1,
                  0,
                ),
                child: Row(
                  children: [
                    Icon(categoryIcon(cat), color: categoryColor(cat)),
                    const SizedBox(width: HkTokens.s2),
                    Expanded(
                      child: Text(
                        info?.displayName ?? shortType(node.type),
                        style: text.titleMedium,
                        overflow: TextOverflow.ellipsis,
                      ),
                    ),
                    IconButton(
                      tooltip: 'Close panel',
                      icon: const Icon(Icons.close),
                      onPressed: onClose,
                    ),
                  ],
                ),
              ),
              Padding(
                padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
                child: _NameField(editor: editor, name: node.name),
              ),
              if (unsupported)
                Container(
                  margin: const EdgeInsets.all(HkTokens.s3),
                  padding: const EdgeInsets.all(HkTokens.s3),
                  decoration: BoxDecoration(
                    color: hk.warn.withValues(alpha: 0.12),
                    borderRadius: BorderRadius.circular(HkTokens.radiusCard),
                    border: Border.all(color: hk.warn),
                  ),
                  child: Row(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Icon(
                        Icons.warning_amber_rounded,
                        color: hk.warn,
                        size: 18,
                      ),
                      const SizedBox(width: HkTokens.s2),
                      Expanded(
                        child: Text(
                          info == null
                              ? '${node.type} is not available in this build. The node is kept '
                                    'exactly as imported and exports unchanged; running it fails here.'
                              : 'typeVersion ${node.typeVersion} is not implemented '
                                    '(supported: ${info.versions.join(', ')}). Kept as-is.',
                          style: text.bodySmall,
                        ),
                      ),
                    ],
                  ),
                ),
              const TabBar(
                isScrollable: true,
                tabAlignment: TabAlignment.start,
                tabs: [
                  Tab(text: 'Parameters'),
                  Tab(text: 'Input'),
                  Tab(text: 'Output'),
                  Tab(text: 'Settings'),
                  Tab(text: 'JSON'),
                ],
              ),
              Expanded(
                child: TabBarView(
                  children: [
                    _ParamsTab(editor: editor, node: node, info: info),
                    NodeIoTab(
                      editor: editor,
                      nodeName: node.name,
                      overlay: overlay,
                      side: IoSide.input,
                    ),
                    NodeIoTab(
                      editor: editor,
                      nodeName: node.name,
                      overlay: overlay,
                      side: IoSide.output,
                    ),
                    _SettingsTab(editor: editor, node: node, info: info),
                    _JsonTab(editor: editor, node: node),
                  ],
                ),
              ),
            ],
          ),
        );
      },
    );
  }
}

class _NameField extends StatefulWidget {
  const _NameField({required this.editor, required this.name});
  final FlowEditor editor;
  final String name;

  @override
  State<_NameField> createState() => _NameFieldState();
}

class _NameFieldState extends State<_NameField> {
  late final TextEditingController _c = TextEditingController(
    text: widget.name,
  );
  final _focus = FocusNode();
  String? _error;

  @override
  void initState() {
    super.initState();
    _focus.addListener(() {
      if (!_focus.hasFocus) _commit();
    });
  }

  @override
  void didUpdateWidget(_NameField old) {
    super.didUpdateWidget(old);
    if (!_focus.hasFocus && _c.text != widget.name) _c.text = widget.name;
  }

  @override
  void dispose() {
    _c.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _commit() {
    final t = _c.text.trim();
    if (t == widget.name) {
      if (_error != null) setState(() => _error = null);
      return;
    }
    if (widget.editor.renameNode(widget.name, t)) {
      setState(() => _error = null);
    } else {
      setState(
        () => _error = t.isEmpty ? 'Name required' : 'Name already used',
      );
    }
  }

  @override
  Widget build(BuildContext context) => TextField(
    controller: _c,
    focusNode: _focus,
    decoration: InputDecoration(
      labelText: 'Node name',
      errorText: _error,
      isDense: true,
    ),
    onSubmitted: (_) => _commit(),
  );
}

class _ParamsTab extends StatelessWidget {
  const _ParamsTab({
    required this.editor,
    required this.node,
    required this.info,
  });
  final FlowEditor editor;
  final FlowNode node;
  final NodeTypeInfo? info;

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        if (info != null)
          for (final c in info!.credentials)
            if (c.isVisible(node.parameters, typeVersion: node.typeVersion))
              _CredentialPicker(editor: editor, node: node, req: c),
        if (info != null)
          ParamsForm(
            specs: info!.properties,
            values: node.parameters,
            typeVersion: node.typeVersion,
            onChanged: (next, key) =>
                editor.setParameters(node.name, next, coalesce: key),
          )
        else
          JsonValueField(
            key: ValueKey('raw-${node.name}'),
            label: 'parameters',
            help: 'No form for this node type: edit the raw n8n parameters.',
            value: node.parameters,
            onChanged: (v) {
              if (v is Map<String, dynamic>) {
                editor.setParameters(node.name, v, coalesce: 'raw');
              }
            },
          ),
      ],
    );
  }
}

/// Pick (or create) the credential a node uses for one credential type. Stored in the node's
/// n8n `credentials` map as `{id, name}`; the secret values stay in the secret store.
class _CredentialPicker extends ConsumerWidget {
  const _CredentialPicker({
    required this.editor,
    required this.node,
    required this.req,
  });
  final FlowEditor editor;
  final FlowNode node;
  final CredReq req;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final all = ref.watch(credentialBookProvider).value ?? const [];
    final types = ref.watch(credentialTypesProvider).value ?? const [];
    final mine = all.where((c) => c.type == req.name).toList();
    final typeLabel =
        types.where((t) => t.name == req.name).firstOrNull?.displayName ??
        req.name;
    final cur = node.credentials[req.name];
    final curId = cur?['id'] as String?;
    final known = mine.any((c) => c.id == curId);
    void pick(String? id) {
      final name = mine.where((c) => c.id == id).firstOrNull?.name;
      editor.updateNode(
        node.name,
        (n) => n.withCredential(req.name, id: id, name: name),
      );
    }

    return Padding(
      padding: const EdgeInsets.only(bottom: HkTokens.s3),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          Expanded(
            child: DropdownButtonFormField<String?>(
              key: ValueKey('cred-${node.name}-${req.name}-$curId'),
              initialValue: known ? curId : null,
              isExpanded: true,
              decoration: InputDecoration(
                labelText: req.required
                    ? 'Credential: $typeLabel *'
                    : 'Credential: $typeLabel',
                helperText: curId != null && !known
                    ? 'Saved credential "${cur?['name'] ?? curId}" not found on this device'
                    : null,
              ),
              items: [
                const DropdownMenuItem<String?>(
                  value: null,
                  child: Text('None'),
                ),
                for (final c in mine)
                  DropdownMenuItem<String?>(value: c.id, child: Text(c.name)),
              ],
              onChanged: pick,
            ),
          ),
          IconButton(
            tooltip: 'New credential',
            icon: const Icon(Icons.add),
            onPressed: () async {
              final made = await showCredentialDialog(
                context,
                fixedType: req.name,
              );
              if (made != null) {
                editor.updateNode(
                  node.name,
                  (n) =>
                      n.withCredential(req.name, id: made.id, name: made.name),
                );
              }
            },
          ),
        ],
      ),
    );
  }
}

class _SettingsTab extends StatelessWidget {
  const _SettingsTab({
    required this.editor,
    required this.node,
    required this.info,
  });
  final FlowEditor editor;
  final FlowNode node;
  final NodeTypeInfo? info;

  Widget _flag(String title, String key, {String? sub}) => SwitchListTile(
    contentPadding: EdgeInsets.zero,
    dense: true,
    title: Text(title),
    subtitle: sub == null ? null : Text(sub),
    value: node.raw[key] == true,
    onChanged: (v) => editor.updateNode(
      node.name,
      (n) => v ? n.set(key, true) : n.remove(key),
    ),
  );

  @override
  Widget build(BuildContext context) {
    final versions = info?.versions ?? const <double>[];
    final cur = node.typeVersion.toDouble();
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        _flag(
          'Disabled',
          'disabled',
          sub: 'Skipped at run time; data passes through',
        ),
        _flag('Always output data', 'alwaysOutputData'),
        _flag('Execute once', 'executeOnce'),
        _flag('Retry on fail', 'retryOnFail'),
        const SizedBox(height: HkTokens.s2),
        DropdownButtonFormField<String>(
          key: ValueKey('onerr-${node.raw['onError']}'),
          initialValue: (node.raw['onError'] as String?) ?? 'stopWorkflow',
          decoration: const InputDecoration(
            labelText: 'On error',
            isDense: true,
          ),
          items: const [
            DropdownMenuItem(
              value: 'stopWorkflow',
              child: Text('Stop workflow'),
            ),
            DropdownMenuItem(
              value: 'continueRegularOutput',
              child: Text('Continue'),
            ),
            DropdownMenuItem(
              value: 'continueErrorOutput',
              child: Text('Continue (error output)'),
            ),
          ],
          onChanged: (v) => editor.updateNode(
            node.name,
            (n) => v == null || v == 'stopWorkflow'
                ? n.remove('onError')
                : n.set('onError', v),
          ),
        ),
        if (versions.length > 1) ...[
          const SizedBox(height: HkTokens.s3),
          DropdownButtonFormField<double>(
            key: ValueKey('ver-$cur'),
            initialValue: versions.contains(cur) ? cur : null,
            decoration: const InputDecoration(
              labelText: 'Node version',
              isDense: true,
            ),
            items: [
              for (final v in versions)
                DropdownMenuItem(
                  value: v,
                  child: Text(v == v.roundToDouble() ? '${v.toInt()}' : '$v'),
                ),
            ],
            onChanged: (v) {
              if (v == null) return;
              editor.updateNode(
                node.name,
                (n) => n.set(
                  'typeVersion',
                  v == v.roundToDouble() ? v.toInt() : v,
                ),
              );
            },
          ),
        ],
        const SizedBox(height: HkTokens.s3),
        TextFormField(
          key: ValueKey('notes-${node.name}'),
          initialValue: node.notes ?? '',
          minLines: 2,
          maxLines: 5,
          decoration: const InputDecoration(
            labelText: 'Notes',
            isDense: true,
            alignLabelWithHint: true,
          ),
          onChanged: (v) => editor.updateNode(
            node.name,
            (n) => v.isEmpty ? n.remove('notes') : n.set('notes', v),
            coalesce: 'notes',
          ),
        ),
      ],
    );
  }
}

class _JsonTab extends StatelessWidget {
  const _JsonTab({required this.editor, required this.node});
  final FlowEditor editor;
  final FlowNode node;

  @override
  Widget build(BuildContext context) {
    final json = exportNodes(editor.workflow, [node.name]);
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        Align(
          alignment: Alignment.centerRight,
          child: TextButton.icon(
            icon: const Icon(Icons.copy, size: 16),
            label: const Text('Copy as n8n JSON'),
            onPressed: () async {
              final messenger = ScaffoldMessenger.maybeOf(context);
              await Clipboard.setData(ClipboardData(text: json));
              messenger?.showSnackBar(const SnackBar(content: Text('Copied')));
            },
          ),
        ),
        SelectableText(
          json,
          style: const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 12),
        ),
      ],
    );
  }
}
