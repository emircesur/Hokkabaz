import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../theme/tokens.dart';
import 'credential_dialogs.dart';
import 'n8n_tab.dart';
import 'plugins_models.dart';
import 'plugins_providers.dart';

/// DESIGN §4.5 Plugins: WASM plugins (capability-gated), MCP servers (trust-gated), credentials.
class PluginsScreen extends ConsumerWidget {
  const PluginsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final backendError = ref.watch(backendErrorProvider);
    final text = Theme.of(context).textTheme;
    final hk = context.hk;
    return DefaultTabController(
      length: 4,
      child: Align(
        alignment: Alignment.topCenter,
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 900),
          child: Column(
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(HkTokens.s4, HkTokens.s4, HkTokens.s4, 0),
                child: Align(
                  alignment: Alignment.centerLeft,
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('Plugins', style: text.titleLarge),
                      Text(
                        'Extend the agent and flows with sandboxed plugins, MCP servers and credentials.',
                        style: text.bodySmall?.copyWith(color: hk.textMuted),
                      ),
                    ],
                  ),
                ),
              ),
              const TabBar(
                isScrollable: true,
                tabAlignment: TabAlignment.start,
                tabs: [Tab(text: 'Plugins'), Tab(text: 'MCP servers'), Tab(text: 'Credentials'), Tab(text: 'n8n')],
              ),
              Expanded(
                child: backendError != null
                    ? Padding(
                        padding: const EdgeInsets.all(HkTokens.s4),
                        child: Text(backendError, style: TextStyle(color: hk.err)),
                      )
                    : const TabBarView(children: [_PluginsTab(), _McpTab(), _CredentialsTab(), N8nTab()]),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

void _toast(BuildContext context, String msg) => ScaffoldMessenger.of(context)
  ..hideCurrentSnackBar()
  ..showSnackBar(SnackBar(content: Text(msg)));

Future<bool> _confirm(BuildContext context, String title, String body, String action) async =>
    await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(title),
        content: Text(body),
        actions: [
          TextButton(onPressed: () => Navigator.pop(ctx, false), child: const Text('Cancel')),
          FilledButton(onPressed: () => Navigator.pop(ctx, true), child: Text(action)),
        ],
      ),
    ) ??
    false;

Widget _chips(BuildContext context, Iterable<String> items, {Color? color}) => Wrap(
  spacing: HkTokens.s1,
  runSpacing: HkTokens.s1,
  children: [
    for (final i in items)
      Chip(
        visualDensity: VisualDensity.compact,
        label: Text(i, style: Theme.of(context).textTheme.labelSmall),
        side: color == null ? null : BorderSide(color: color),
      ),
  ],
);

// ---------------------------------------------------------------- plugins

class _PluginsTab extends ConsumerWidget {
  const _PluginsTab();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final book = ref.watch(pluginBookProvider);
    final hk = context.hk;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Wrap(
          spacing: HkTokens.s2,
          runSpacing: HkTokens.s2,
          children: [
            FilledButton.icon(
              icon: const Icon(Icons.store_outlined, size: 18),
              label: const Text('Browse registry'),
              onPressed: () => showDialog<void>(context: context, builder: (_) => const _RegistryDialog()),
            ),
            OutlinedButton.icon(
              icon: const Icon(Icons.upload_file, size: 18),
              label: const Text('Install from file'),
              onPressed: () async {
                final r = await FilePicker.pickFiles(type: FileType.any);
                final path = r.firstOrNull?.path;
                if (path == null || !context.mounted) return;
                final err = await ref.read(pluginBookProvider.notifier).installFile(path);
                if (context.mounted) _toast(context, err ?? 'Installed');
              },
            ),
          ],
        ),
        const SizedBox(height: HkTokens.s3),
        book.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (list) => list.isEmpty
              ? Text(
                  'No plugins installed. Plugins run in a WASM sandbox and can only reach '
                  'the hosts, secrets and storage their manifest declares.',
                  style: TextStyle(color: hk.textMuted),
                )
              : Column(
                  children: [
                    for (final p in list)
                      Padding(padding: const EdgeInsets.only(bottom: HkTokens.s2), child: _PluginCard(p)),
                  ],
                ),
        ),
      ],
    );
  }
}

class _PluginCard extends ConsumerWidget {
  const _PluginCard(this.p);
  final PluginInfo p;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final notifier = ref.read(pluginBookProvider.notifier);
    return Container(
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: p.error != null ? hk.err : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(child: Text('${p.name}  v${p.version}', style: text.titleSmall)),
              Switch(
                value: p.enabled,
                onChanged: (v) async {
                  final err = await notifier.setEnabled(p.id, v);
                  if (context.mounted && err != null) _toast(context, err);
                },
              ),
              IconButton(
                tooltip: 'Remove',
                icon: const Icon(Icons.delete_outline),
                onPressed: () async {
                  if (!await _confirm(context, 'Remove ${p.name}?', 'Its files and settings are deleted.', 'Remove')) return;
                  final err = await notifier.remove(p.id);
                  if (context.mounted && err != null) _toast(context, err);
                },
              ),
            ],
          ),
          if (p.description.isNotEmpty) Text(p.description, style: text.bodySmall?.copyWith(color: hk.textMuted)),
          if (p.error != null)
            Padding(
              padding: const EdgeInsets.only(top: HkTokens.s1),
              child: Text(p.error!, style: text.bodySmall?.copyWith(color: hk.err)),
            ),
          const SizedBox(height: HkTokens.s2),
          Text('Can access', style: text.labelSmall?.copyWith(color: hk.textMuted)),
          p.capabilities.isEmpty
              ? Text('Nothing beyond its own computation', style: text.bodySmall)
              : _chips(context, p.capabilities, color: hk.warn),
          if (p.tools.isNotEmpty || p.nodes.isNotEmpty || p.triggers.isNotEmpty) ...[
            const SizedBox(height: HkTokens.s2),
            Text('Provides', style: text.labelSmall?.copyWith(color: hk.textMuted)),
            _chips(context, [
              ...p.tools.map((t) => 'tool: $t'),
              ...p.nodes.map((n) => 'node: $n'),
              ...p.triggers.map((t) => 'trigger: $t'),
            ]),
          ],
          if (p.settingsSchema != null)
            Align(
              alignment: Alignment.centerRight,
              child: TextButton.icon(
                icon: const Icon(Icons.tune, size: 18),
                label: const Text('Settings'),
                onPressed: () => showDialog<void>(context: context, builder: (_) => _SettingsDialog(p)),
              ),
            ),
        ],
      ),
    );
  }
}

/// Plugin settings are plugin-defined JSON; edited as raw JSON validated for syntax here.
class _SettingsDialog extends ConsumerStatefulWidget {
  const _SettingsDialog(this.p);
  final PluginInfo p;

  @override
  ConsumerState<_SettingsDialog> createState() => _SettingsDialogState();
}

class _SettingsDialogState extends ConsumerState<_SettingsDialog> {
  late final _c = TextEditingController(text: const JsonEncoder.withIndent('  ').convert(widget.p.settings));
  String? _error;

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    Map<String, dynamic> v;
    try {
      v = (jsonDecode(_c.text) as Map).cast();
    } catch (e) {
      setState(() => _error = 'Not a JSON object');
      return;
    }
    final err = await ref.read(pluginBookProvider.notifier).setSettings(widget.p.id, v);
    if (!mounted) return;
    if (err == null) {
      Navigator.pop(context);
    } else {
      setState(() => _error = err);
    }
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('${widget.p.name} settings'),
    content: SizedBox(
      width: 480,
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (widget.p.settingsSchema?['description'] is String)
            Padding(
              padding: const EdgeInsets.only(bottom: HkTokens.s2),
              child: Text(widget.p.settingsSchema!['description'] as String),
            ),
          TextField(
            controller: _c,
            maxLines: 12,
            style: const TextStyle(fontFamily: 'monospace'),
            decoration: InputDecoration(errorText: _error),
          ),
        ],
      ),
    ),
    actions: [
      TextButton(onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
      FilledButton(onPressed: _save, child: const Text('Save')),
    ],
  );
}

class _RegistryDialog extends ConsumerWidget {
  const _RegistryDialog();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final reg = ref.watch(registryProvider);
    final hk = context.hk;
    return AlertDialog(
      title: const Text('Plugin registry'),
      content: SizedBox(
        width: 520,
        height: 420,
        child: reg.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (list) => list.isEmpty
              ? const Text('The registry has no plugins yet.')
              : ListView(
                  children: [
                    for (final e in list)
                      ListTile(
                        title: Text('${e.name}  v${e.version}'),
                        subtitle: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            Text(e.description),
                            if (e.capabilities.isNotEmpty) Text('Access: ${e.capabilities.join(' · ')}', style: TextStyle(color: hk.warn)),
                          ],
                        ),
                        isThreeLine: true,
                        trailing: FilledButton(
                          onPressed: e.installedVersion != null && !e.updateAvailable
                              ? null
                              : () async {
                                  if (!await _confirm(
                                    context,
                                    'Install ${e.name}?',
                                    e.capabilities.isEmpty ? 'It declares no access to the network, secrets or storage.' : 'It can access: ${e.capabilities.join(', ')}.',
                                    e.updateAvailable ? 'Update' : 'Install',
                                  )) {
                                    return;
                                  }
                                  final err = await ref.read(pluginBookProvider.notifier).installFromRegistry(e.id, indexUrl: ref.read(registryUrlProvider));
                                  if (context.mounted) _toast(context, err ?? '${e.name} installed');
                                },
                          child: Text(e.installedVersion == null ? 'Install' : e.updateAvailable ? 'Update' : 'Installed'),
                        ),
                      ),
                  ],
                ),
        ),
      ),
      actions: [TextButton(onPressed: () => Navigator.pop(context), child: const Text('Close'))],
    );
  }
}

// -------------------------------------------------------------------- MCP

class _McpTab extends ConsumerWidget {
  const _McpTab();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final book = ref.watch(mcpBookProvider);
    final hk = context.hk;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: FilledButton.icon(
            icon: const Icon(Icons.add, size: 18),
            label: const Text('Add MCP server'),
            onPressed: () => showDialog<void>(context: context, builder: (_) => const _McpDialog()),
          ),
        ),
        const SizedBox(height: HkTokens.s3),
        book.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (list) => list.isEmpty
              ? Text('No MCP servers. Their tools join the agent after you trust them.', style: TextStyle(color: hk.textMuted))
              : Column(children: [for (final s in list) Padding(padding: const EdgeInsets.only(bottom: HkTokens.s2), child: _McpCard(s))]),
        ),
      ],
    );
  }
}

class _McpCard extends ConsumerWidget {
  const _McpCard(this.s);
  final McpServer s;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final n = ref.read(mcpBookProvider.notifier);
    Future<void> run(Future<String?> f) async {
      final err = await f;
      if (context.mounted && err != null) _toast(context, err);
    }

    return Container(
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: s.error != null ? hk.err : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.circle, size: 10, color: s.connected ? hk.ok : hk.textMuted),
              const SizedBox(width: HkTokens.s2),
              Expanded(child: Text(s.name, style: text.titleSmall)),
              Switch(value: s.enabled, onChanged: (v) => run(n.setEnabled(s.name, v))),
              IconButton(
                tooltip: 'Remove',
                icon: const Icon(Icons.delete_outline),
                onPressed: () async {
                  if (await _confirm(context, 'Remove ${s.name}?', 'Its stored secrets are deleted.', 'Remove')) await run(n.remove(s.name));
                },
              ),
            ],
          ),
          Text('${s.transport}: ${s.target}', style: text.bodySmall?.copyWith(color: hk.textMuted, fontFamily: 'monospace')),
          if (s.transport == 'stdio' && !s.stdioSupported)
            Text('stdio servers cannot run on Android; use an HTTP server.', style: text.bodySmall?.copyWith(color: hk.warn)),
          if (s.error != null) Text(s.error!, style: text.bodySmall?.copyWith(color: hk.err)),
          if (s.tools.isNotEmpty) ...[const SizedBox(height: HkTokens.s2), _chips(context, s.tools)],
          const SizedBox(height: HkTokens.s2),
          if (!s.trusted)
            Row(
              children: [
                Icon(Icons.shield_outlined, size: 16, color: hk.warn),
                const SizedBox(width: HkTokens.s1),
                Expanded(child: Text('Untrusted: not connected.', style: text.bodySmall?.copyWith(color: hk.warn))),
                FilledButton(
                  onPressed: () async {
                    final what = s.transport == 'stdio' ? 'run this command on your device:\n${s.target}' : 'send requests to ${s.target}';
                    if (await _confirm(context, 'Trust ${s.name}?', 'Trusting lets the agent $what and call any tool the server exposes.', 'Trust')) {
                      await run(n.setTrusted(s.name, true));
                    }
                  },
                  child: const Text('Review & trust'),
                ),
              ],
            )
          else
            Row(
              children: [
                Icon(Icons.verified_user_outlined, size: 16, color: hk.ok),
                const SizedBox(width: HkTokens.s1),
                const Text('Trusted'),
                const Spacer(),
                TextButton(onPressed: () => run(n.connect(s.name)), child: const Text('Reconnect')),
                TextButton(onPressed: () => run(n.setTrusted(s.name, false)), child: const Text('Revoke')),
              ],
            ),
        ],
      ),
    );
  }
}

class _McpDialog extends ConsumerStatefulWidget {
  const _McpDialog();

  @override
  ConsumerState<_McpDialog> createState() => _McpDialogState();
}

class _McpDialogState extends ConsumerState<_McpDialog> {
  final _name = TextEditingController();
  final _url = TextEditingController();
  final _token = TextEditingController();
  final _command = TextEditingController();
  final _args = TextEditingController();
  final _env = TextEditingController();
  String _transport = 'http';
  String? _error;
  bool _busy = false;

  @override
  void dispose() {
    for (final c in [_name, _url, _token, _command, _args, _env]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _save() async {
    final spec = <String, dynamic>{'name': _name.text.trim(), 'transport': _transport};
    if (_transport == 'http') {
      spec['url'] = _url.text.trim();
      if (_token.text.isNotEmpty) spec['bearer_token'] = _token.text;
    } else {
      spec['command'] = _command.text.trim();
      spec['args'] = _args.text.trim().isEmpty ? <String>[] : _args.text.trim().split(RegExp(r'\s+'));
      spec['env'] = {
        for (final l in _env.text.split('\n').where((l) => l.contains('=')))
          l.substring(0, l.indexOf('=')).trim(): l.substring(l.indexOf('=') + 1),
      };
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await ref.read(mcpBookProvider.notifier).save(spec);
    if (!mounted) return;
    if (err == null) {
      Navigator.pop(context);
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final stdioOk = !const bool.fromEnvironment('dart.library.js_interop') && !Theme.of(context).platform.name.contains('android');
    return AlertDialog(
      title: const Text('Add MCP server'),
      content: SizedBox(
        width: 460,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              TextField(controller: _name, decoration: const InputDecoration(labelText: 'Name')),
              const SizedBox(height: HkTokens.s2),
              SegmentedButton<String>(
                segments: [
                  const ButtonSegment(value: 'http', label: Text('HTTP')),
                  ButtonSegment(value: 'stdio', label: const Text('Command'), enabled: stdioOk),
                ],
                selected: {_transport},
                onSelectionChanged: (s) => setState(() => _transport = s.first),
              ),
              const SizedBox(height: HkTokens.s2),
              if (_transport == 'http') ...[
                TextField(controller: _url, decoration: const InputDecoration(labelText: 'URL')),
                const SizedBox(height: HkTokens.s2),
                TextField(controller: _token, obscureText: true, decoration: const InputDecoration(labelText: 'Bearer token (optional)')),
              ] else ...[
                TextField(controller: _command, decoration: const InputDecoration(labelText: 'Command')),
                const SizedBox(height: HkTokens.s2),
                TextField(controller: _args, decoration: const InputDecoration(labelText: 'Arguments (space separated)')),
                const SizedBox(height: HkTokens.s2),
                TextField(controller: _env, maxLines: 3, decoration: const InputDecoration(labelText: 'Environment (KEY=value per line; values are stored encrypted)')),
              ],
              if (_error != null)
                Padding(padding: const EdgeInsets.only(top: HkTokens.s2), child: Text(_error!, style: TextStyle(color: context.hk.err))),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(onPressed: _busy ? null : () => Navigator.pop(context), child: const Text('Cancel')),
        FilledButton(onPressed: _busy ? null : _save, child: const Text('Save')),
      ],
    );
  }
}

// ------------------------------------------------------------ credentials

class _CredentialsTab extends ConsumerWidget {
  const _CredentialsTab();

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final book = ref.watch(credentialBookProvider);
    final types = ref.watch(credentialTypesProvider).value ?? const [];
    final hk = context.hk;
    String label(String t) => types.where((x) => x.name == t).firstOrNull?.displayName ?? t;
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Align(
          alignment: Alignment.centerLeft,
          child: FilledButton.icon(
            icon: const Icon(Icons.add, size: 18),
            label: const Text('New credential'),
            onPressed: () => showCredentialDialog(context),
          ),
        ),
        const SizedBox(height: HkTokens.s2),
        Text(
          'Values are stored encrypted on this device and never shown again. Flows reference credentials by name.',
          style: Theme.of(context).textTheme.bodySmall?.copyWith(color: hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s3),
        book.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (list) => list.isEmpty
              ? Text('No credentials yet.', style: TextStyle(color: hk.textMuted))
              : Column(
                  children: [
                    for (final c in list)
                      Card(
                        margin: const EdgeInsets.only(bottom: HkTokens.s2),
                        child: ListTile(
                          leading: const Icon(Icons.key_outlined),
                          title: Text(c.name),
                          subtitle: Text(label(c.type)),
                          trailing: Row(
                            mainAxisSize: MainAxisSize.min,
                            children: [
                              IconButton(tooltip: 'Edit', icon: const Icon(Icons.edit_outlined), onPressed: () => showCredentialDialog(context, existing: c)),
                              IconButton(
                                tooltip: 'Delete',
                                icon: const Icon(Icons.delete_outline),
                                onPressed: () async {
                                  if (!await _confirm(context, 'Delete ${c.name}?', 'Flows using it will fail until you pick another.', 'Delete')) return;
                                  final err = await ref.read(credentialBookProvider.notifier).remove(c.id);
                                  if (context.mounted && err != null) _toast(context, err);
                                },
                              ),
                            ],
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
