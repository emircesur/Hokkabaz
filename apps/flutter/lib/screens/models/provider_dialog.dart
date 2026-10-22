import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../ffi/settings_providers.dart';
import '../../src/rust/api/settings.dart' as rust;
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';

/// Add a custom endpoint (OpenAI-compatible or Ollama) or edit an existing one.
Future<void> showProviderDialog(
  BuildContext context,
  WidgetRef ref, {
  ProviderDto? existing,
}) {
  // Long form + keyboard: a bottom sheet on phones, a dialog on desktop.
  if (isNarrow(context)) {
    return showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      showDragHandle: true,
      builder: (_) => _ProviderDialog(existing: existing, sheet: true),
    );
  }
  return showDialog<void>(
    context: context,
    builder: (_) => _ProviderDialog(existing: existing),
  );
}

class _ProviderDialog extends ConsumerStatefulWidget {
  const _ProviderDialog({this.existing, this.sheet = false});
  final ProviderDto? existing;
  final bool sheet;

  @override
  ConsumerState<_ProviderDialog> createState() => _ProviderDialogState();
}

class _ProviderDialogState extends ConsumerState<_ProviderDialog> {
  late final TextEditingController _id;
  late final TextEditingController _url;
  late final TextEditingController _models;
  late ProviderKindDto _kind;
  bool _saving = false;
  String? _error;
  final List<Map<String, dynamic>> _presets = _loadPresets();

  static List<Map<String, dynamic>> _loadPresets() {
    try {
      return (jsonDecode(rust.providerPresets()) as List)
          .cast<Map<String, dynamic>>();
    } catch (_) {
      return const [];
    }
  }

  void _usePreset(Map<String, dynamic> p) => setState(() {
    _id.text = p['id'] as String;
    _url.text = p['base_url'] as String;
    _kind = (p['kind'] as String) == 'ollama'
        ? ProviderKindDto.ollama
        : ProviderKindDto.openaiCompat;
  });

  @override
  void initState() {
    super.initState();
    final e = widget.existing;
    _id = TextEditingController(text: e?.id ?? '');
    _url = TextEditingController(text: e?.baseUrl ?? '');
    _models = TextEditingController(text: e?.models.join(', ') ?? '');
    _kind = e?.kind ?? ProviderKindDto.openaiCompat;
  }

  @override
  void dispose() {
    _id.dispose();
    _url.dispose();
    _models.dispose();
    super.dispose();
  }

  Future<void> _save() async {
    final id = _id.text.trim();
    final url = _url.text.trim();
    if (id.isEmpty) {
      setState(() => _error = 'Name is required');
      return;
    }
    if (_kind == ProviderKindDto.openaiCompat && url.isEmpty) {
      setState(() => _error = 'Base URL is required');
      return;
    }
    setState(() {
      _saving = true;
      _error = null;
    });
    final err = await ref
        .read(settingsProvider.notifier)
        .saveProvider(
          ProviderInputDto(
            id: id,
            kind: _kind,
            baseUrl: url.isEmpty ? null : url,
            enabled: widget.existing?.enabled ?? true,
            models: _models.text
                .split(',')
                .map((s) => s.trim())
                .where((s) => s.isNotEmpty)
                .toList(),
          ),
        );
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _saving = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final editing = widget.existing != null;
    final builtin = widget.existing?.builtin ?? false;
    final title = editing ? 'Edit endpoint' : 'Add endpoint';
    final form = Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        if (!editing && _presets.isNotEmpty)
          Padding(
            padding: const EdgeInsets.only(bottom: HkTokens.s3),
            child: Wrap(
              spacing: HkTokens.s2,
              runSpacing: HkTokens.s1,
              children: [
                for (final p in _presets)
                  ActionChip(
                    label: Text(p['label'] as String),
                    avatar: (p['local'] as bool)
                        ? const Icon(Icons.computer, size: 16)
                        : null,
                    onPressed: () => _usePreset(p),
                  ),
              ],
            ),
          ),
        if (!editing)
          DropdownButtonFormField<ProviderKindDto>(
            initialValue: _kind,
            decoration: const InputDecoration(labelText: 'Type'),
            items: const [
              DropdownMenuItem(
                value: ProviderKindDto.openaiCompat,
                child: Text('OpenAI-compatible'),
              ),
              DropdownMenuItem(
                value: ProviderKindDto.ollama,
                child: Text('Ollama'),
              ),
            ],
            onChanged: (v) => setState(() => _kind = v ?? _kind),
          ),
        TextField(
          controller: _id,
          enabled: !editing,
          decoration: const InputDecoration(
            labelText: 'Name',
            hintText: 'e.g. openrouter',
          ),
        ),
        TextField(
          controller: _url,
          keyboardType: TextInputType.url,
          decoration: InputDecoration(
            labelText: 'Base URL',
            hintText: _kind == ProviderKindDto.ollama
                ? 'http://localhost:11434'
                : 'https://api.example.com/v1',
          ),
        ),
        if (!builtin)
          TextField(
            controller: _models,
            decoration: const InputDecoration(
              labelText: 'Model ids (optional)',
              helperText: 'Comma separated; empty asks the provider',
            ),
          ),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 12),
            child: Text(_error!, style: TextStyle(color: context.hk.err)),
          ),
      ],
    );
    final cancel = TextButton(
      style: widget.sheet
          ? TextButton.styleFrom(minimumSize: const Size(64, kTouchTarget))
          : null,
      onPressed: _saving ? null : () => Navigator.of(context).pop(),
      child: const Text('Cancel'),
    );
    final save = FilledButton(
      style: widget.sheet
          ? FilledButton.styleFrom(minimumSize: const Size(88, kTouchTarget))
          : null,
      onPressed: _saving ? null : _save,
      child: const Text('Save'),
    );
    if (widget.sheet) {
      return Padding(
        padding: EdgeInsets.only(
          bottom: MediaQuery.viewInsetsOf(context).bottom,
        ),
        child: SafeArea(
          child: SingleChildScrollView(
            padding: const EdgeInsets.fromLTRB(
              HkTokens.s4,
              0,
              HkTokens.s4,
              HkTokens.s4,
            ),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(title, style: Theme.of(context).textTheme.titleLarge),
                const SizedBox(height: HkTokens.s3),
                form,
                const SizedBox(height: HkTokens.s3),
                Row(
                  mainAxisAlignment: MainAxisAlignment.end,
                  children: [
                    cancel,
                    const SizedBox(width: HkTokens.s2),
                    save,
                  ],
                ),
              ],
            ),
          ),
        ),
      );
    }
    return AlertDialog(
      title: Text(title),
      content: SizedBox(width: 420, child: SingleChildScrollView(child: form)),
      actions: [cancel, save],
    );
  }
}
