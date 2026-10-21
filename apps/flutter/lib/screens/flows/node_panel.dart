import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../theme/tokens.dart';
import 'model/flow_editor.dart';
import 'model/n8n_clipboard.dart';
import 'model/node_types.dart';
import 'model/workflow_model.dart';
import 'node_card.dart';
import 'params_form.dart';

/// Right-hand inspector for one node: rename, generated params form, node
/// settings and the raw n8n JSON (DESIGN §4.4 "node panel").
class NodePanel extends StatelessWidget {
  const NodePanel({
    super.key,
    required this.editor,
    required this.nodeName,
    required this.types,
    required this.onClose,
  });

  final FlowEditor editor;
  final String nodeName;
  final Map<String, NodeTypeInfo> types;
  final VoidCallback onClose;

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
        final unsupported = info == null || !info.supportsVersion(node.typeVersion);
        return DefaultTabController(
          length: 3,
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(HkTokens.s3, HkTokens.s3, HkTokens.s1, 0),
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
                      Icon(Icons.warning_amber_rounded, color: hk.warn, size: 18),
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
                tabs: [Tab(text: 'Parameters'), Tab(text: 'Settings'), Tab(text: 'JSON')],
              ),
              Expanded(
                child: TabBarView(
                  children: [
                    _ParamsTab(editor: editor, node: node, info: info),
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
  late final TextEditingController _c = TextEditingController(text: widget.name);
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
      setState(() => _error = t.isEmpty ? 'Name required' : 'Name already used');
    }
  }

  @override
  Widget build(BuildContext context) => TextField(
    controller: _c,
    focusNode: _focus,
    decoration: InputDecoration(labelText: 'Node name', errorText: _error, isDense: true),
    onSubmitted: (_) => _commit(),
  );
}

class _ParamsTab extends StatelessWidget {
  const _ParamsTab({required this.editor, required this.node, required this.info});
  final FlowEditor editor;
  final FlowNode node;
  final NodeTypeInfo? info;

  @override
  Widget build(BuildContext context) {
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s3),
      children: [
        if (info != null)
          ParamsForm(
            specs: info!.properties,
            values: node.parameters,
            typeVersion: node.typeVersion,
            onChanged: (next, key) => editor.setParameters(node.name, next, coalesce: key),
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

class _SettingsTab extends StatelessWidget {
  const _SettingsTab({required this.editor, required this.node, required this.info});
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
        _flag('Disabled', 'disabled', sub: 'Skipped at run time; data passes through'),
        _flag('Always output data', 'alwaysOutputData'),
        _flag('Execute once', 'executeOnce'),
        _flag('Retry on fail', 'retryOnFail'),
        const SizedBox(height: HkTokens.s2),
        DropdownButtonFormField<String>(
          initialValue: (node.raw['onError'] as String?) ?? 'stopWorkflow',
          decoration: const InputDecoration(labelText: 'On error', isDense: true),
          items: const [
            DropdownMenuItem(value: 'stopWorkflow', child: Text('Stop workflow')),
            DropdownMenuItem(value: 'continueRegularOutput', child: Text('Continue')),
            DropdownMenuItem(value: 'continueErrorOutput', child: Text('Continue (error output)')),
          ],
          onChanged: (v) => editor.updateNode(
            node.name,
            (n) => v == null || v == 'stopWorkflow' ? n.remove('onError') : n.set('onError', v),
          ),
        ),
        if (versions.length > 1) ...[
          const SizedBox(height: HkTokens.s3),
          DropdownButtonFormField<double>(
            initialValue: versions.contains(cur) ? cur : null,
            decoration: const InputDecoration(labelText: 'Node version', isDense: true),
            items: [
              for (final v in versions)
                DropdownMenuItem(value: v, child: Text(v == v.roundToDouble() ? '${v.toInt()}' : '$v')),
            ],
            onChanged: (v) {
              if (v == null) return;
              editor.updateNode(
                node.name,
                (n) => n.set('typeVersion', v == v.roundToDouble() ? v.toInt() : v),
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
          decoration: const InputDecoration(labelText: 'Notes', isDense: true, alignLabelWithHint: true),
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
