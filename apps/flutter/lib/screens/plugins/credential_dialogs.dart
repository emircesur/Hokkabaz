import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'package:url_launcher/url_launcher.dart';

import '../../src/rust/api/credentials.dart' as cred_api;
import '../../theme/tokens.dart';
import 'plugins_models.dart';
import 'plugins_providers.dart';

/// Create a credential (pass [fixedType]) or pick a type first. With [existing] it renames and/or
/// replaces the stored values; values are never shown, so replacing means re-entering them all.
Future<CredInfo?> showCredentialDialog(
  BuildContext context, {
  String? fixedType,
  CredInfo? existing,
}) => showDialog<CredInfo>(
  context: context,
  builder: (_) => _CredentialDialog(
    fixedType: fixedType ?? existing?.type,
    existing: existing,
  ),
);

class _CredentialDialog extends ConsumerStatefulWidget {
  const _CredentialDialog({this.fixedType, this.existing});
  final String? fixedType;
  final CredInfo? existing;

  @override
  ConsumerState<_CredentialDialog> createState() => _CredentialDialogState();
}

class _CredentialDialogState extends ConsumerState<_CredentialDialog> {
  final _name = TextEditingController();
  final _values = <String, TextEditingController>{};
  String? _type;
  bool _replace = false;
  bool _busy = false;
  String? _error;
  String? _oauthState;
  String? _signInNote;

  @override
  void initState() {
    super.initState();
    _type = widget.fixedType;
    _name.text = widget.existing?.name ?? '';
    _replace = widget.existing == null;
  }

  @override
  void dispose() {
    if (_oauthState != null) cred_api.oauthCancel(state: _oauthState!);
    _name.dispose();
    for (final c in _values.values) {
      c.dispose();
    }
    super.dispose();
  }

  TextEditingController _ctl(CredField f) => _values.putIfAbsent(
    f.name,
    () => TextEditingController(
      text: f.defaultValue is String ? f.defaultValue as String : '',
    ),
  );

  Map<String, dynamic> _collect(CredType t) {
    final out = <String, dynamic>{};
    for (final f in t.fields) {
      final raw = _ctl(f).text;
      if (raw.isEmpty && !f.required) continue;
      out[f.name] = switch (f.type) {
        'number' => num.tryParse(raw) ?? raw,
        'boolean' => raw == 'true',
        _ => raw,
      };
    }
    return out;
  }

  /// Browser sign-in for OAuth credential types: opens the provider's page, waits for the redirect
  /// to come back to this machine, and fills the token fields.
  Future<void> _signIn(CredType t) async {
    setState(() {
      _error = null;
      _signInNote = null;
    });
    try {
      final data = <String, dynamic>{};
      for (final f in t.fields) {
        final v = _ctl(f).text;
        if (v.isNotEmpty) data[f.name] = v;
      }
      final b = jsonDecode(
        await cred_api.oauthBegin(kind: t.name, dataJson: jsonEncode(data)),
      ) as Map<String, dynamic>;
      _oauthState = b['state'] as String;
      setState(
        () => _signInNote =
            'Waiting for the browser. The provider app must allow the redirect ${b['redirect_uri']}',
      );
      await launchUrl(
        Uri.parse(b['auth_url'] as String),
        mode: LaunchMode.externalApplication,
      );
      final tokens = jsonDecode(
        await cred_api.oauthFinish(state: _oauthState!),
      ) as Map<String, dynamic>;
      _oauthState = null;
      for (final f in t.fields) {
        final v = tokens[f.name];
        if (v is String && v.isNotEmpty) _ctl(f).text = v;
      }
      if (mounted)
        setState(() => _signInNote = 'Signed in. Press Save to keep it.');
    } catch (e) {
      _oauthState = null;
      if (mounted) {
        setState(() {
          _signInNote = null;
          _error = '$e';
        });
      }
    }
  }

  Future<void> _save(CredType t) async {
    final missing = _replace
        ? t.fields
              .where((f) => f.required && _ctl(f).text.isEmpty)
              .map((f) => f.label)
        : const <String>[];
    if (_name.text.trim().isEmpty) {
      setState(() => _error = 'Name is required');
      return;
    }
    if (missing.isNotEmpty) {
      setState(() => _error = 'Required: ${missing.join(', ')}');
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    final book = ref.read(credentialBookProvider.notifier);
    final old = widget.existing;
    String? err;
    CredInfo? made = old;
    if (old == null) {
      final r = await book.create(_name.text.trim(), t.name, _collect(t));
      err = r.error;
      made = r.info;
    } else {
      err = await book.edit(
        old.id,
        name: _name.text.trim(),
        data: _replace ? _collect(t) : null,
      );
    }
    if (!mounted) return;
    if (err != null) {
      setState(() {
        _busy = false;
        _error = err;
      });
      return;
    }
    Navigator.of(context).pop(made);
  }

  @override
  Widget build(BuildContext context) {
    final types = ref.watch(credentialTypesProvider);
    final hk = context.hk;
    return AlertDialog(
      title: Text(
        widget.existing == null ? 'New credential' : 'Edit credential',
      ),
      content: SizedBox(
        width: 460,
        child: types.when(
          loading: () => const SizedBox(
            height: 80,
            child: Center(child: CircularProgressIndicator()),
          ),
          error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
          data: (all) {
            final t = all.where((x) => x.name == _type).firstOrNull;
            return SingleChildScrollView(
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (widget.fixedType == null)
                    DropdownMenu<String>(
                      width: 460 - 48,
                      label: const Text('Type'),
                      enableFilter: true,
                      requestFocusOnTap: true,
                      initialSelection: _type,
                      dropdownMenuEntries: [
                        for (final x in all)
                          DropdownMenuEntry(
                            value: x.name,
                            label: x.displayName,
                          ),
                      ],
                      onSelected: (v) => setState(() {
                        _type = v;
                        _values.clear();
                      }),
                    )
                  else
                    Text(
                      t?.displayName ?? widget.fixedType!,
                      style: Theme.of(context).textTheme.titleSmall,
                    ),
                  const SizedBox(height: HkTokens.s3),
                  TextField(
                    controller: _name,
                    decoration: const InputDecoration(labelText: 'Name'),
                  ),
                  if (t != null) ...[
                    if (widget.existing != null)
                      SwitchListTile(
                        contentPadding: EdgeInsets.zero,
                        title: const Text('Replace stored values'),
                        subtitle: const Text(
                          'Values are never shown; re-enter all fields to change them.',
                        ),
                        value: _replace,
                        onChanged: (v) => setState(() => _replace = v),
                      ),
                    if (_replace &&
                        t.fields.any((f) => f.name == 'authUrl') &&
                        t.fields.any((f) => f.name == 'clientId'))
                      Padding(
                        padding: const EdgeInsets.only(top: HkTokens.s2),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.start,
                          children: [
                            OutlinedButton.icon(
                              onPressed: _busy || _oauthState != null
                                  ? null
                                  : () => _signIn(t),
                              icon: _oauthState != null
                                  ? const SizedBox(
                                      width: 16,
                                      height: 16,
                                      child: CircularProgressIndicator(
                                        strokeWidth: 2,
                                      ),
                                    )
                                  : const Icon(Icons.login, size: 18),
                              label: const Text('Sign in with your account'),
                            ),
                            if (_signInNote != null)
                              Padding(
                                padding: const EdgeInsets.only(top: 4),
                                child: Text(
                                  _signInNote!,
                                  style: Theme.of(context).textTheme.bodySmall,
                                ),
                              ),
                            const Text(
                              'Fill Client ID (and secret) of your own app first.',
                              style: TextStyle(fontSize: 12),
                            ),
                          ],
                        ),
                      ),
                    if (_replace)
                      for (final f in t.fields)
                        Padding(
                          padding: const EdgeInsets.only(top: HkTokens.s2),
                          child: _field(f),
                        ),
                  ],
                  if (_error != null)
                    Padding(
                      padding: const EdgeInsets.only(top: HkTokens.s2),
                      child: Text(_error!, style: TextStyle(color: hk.err)),
                    ),
                ],
              ),
            );
          },
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy || _type == null
              ? null
              : () => _save(
                  ref
                      .read(credentialTypesProvider)
                      .requireValue
                      .firstWhere((x) => x.name == _type),
                ),
          child: Text(_busy ? 'Saving…' : 'Save'),
        ),
      ],
    );
  }

  Widget _field(CredField f) {
    final label = f.required ? '${f.label} *' : f.label;
    if (f.type == 'options' && f.options.isNotEmpty) {
      final c = _ctl(f);
      return DropdownMenu<Object?>(
        width: 460 - 48,
        label: Text(label),
        initialSelection: f.options.any((o) => '${o.$1}' == c.text)
            ? f.options.firstWhere((o) => '${o.$1}' == c.text).$1
            : null,
        dropdownMenuEntries: [
          for (final o in f.options)
            DropdownMenuEntry(value: o.$1, label: o.$2),
        ],
        onSelected: (v) => c.text = '${v ?? ''}',
      );
    }
    if (f.type == 'boolean') {
      final c = _ctl(f);
      return Row(
        children: [
          Expanded(child: Text(label)),
          Switch(
            value: c.text == 'true',
            onChanged: (v) => setState(() => c.text = '$v'),
          ),
        ],
      );
    }
    return TextField(
      controller: _ctl(f),
      obscureText: f.secret,
      enableSuggestions: !f.secret,
      autocorrect: false,
      keyboardType: f.type == 'number' ? TextInputType.number : null,
      decoration: InputDecoration(
        labelText: label,
        helperText: f.description,
        helperMaxLines: 2,
      ),
    );
  }
}
