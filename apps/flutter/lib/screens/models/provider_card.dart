import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'provider_dialog.dart';

String kindLabel(ProviderKindDto k) => switch (k) {
  ProviderKindDto.deepseek => 'DeepSeek',
  ProviderKindDto.openaiCompat => 'OpenAI-compatible',
  ProviderKindDto.anthropic => 'Anthropic',
  ProviderKindDto.gemini => 'Gemini',
  ProviderKindDto.ollama => 'Ollama',
};

/// One provider: toggle, key entry, status, and its models.
class ProviderCard extends ConsumerStatefulWidget {
  const ProviderCard({
    super.key,
    required this.provider,
    required this.models,
    required this.isDefaultProvider,
  });

  final ProviderDto provider;
  final AsyncValue<ModelListDto> models;
  final bool isDefaultProvider;

  @override
  ConsumerState<ProviderCard> createState() => _ProviderCardState();
}

class _ProviderCardState extends ConsumerState<ProviderCard> {
  final _key = TextEditingController();
  bool _expanded = false;
  bool _busy = false;
  String? _actionError;

  @override
  void dispose() {
    _key.dispose();
    super.dispose();
  }

  Future<void> _run(Future<String?> Function() op) async {
    setState(() {
      _busy = true;
      _actionError = null;
    });
    final err = await op();
    if (!mounted) return;
    setState(() {
      _busy = false;
      _actionError = err;
    });
  }

  ProviderInputDto _input({bool? enabled}) {
    final p = widget.provider;
    return ProviderInputDto(
      id: p.id,
      kind: p.kind,
      baseUrl: p.baseUrl,
      enabled: enabled ?? p.enabled,
      models: p.models,
    );
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final p = widget.provider;
    final ctl = ref.read(settingsProvider.notifier);
    final listed =
        widget.models.value?.models.where((m) => m.provider == p.id).toList() ??
        const <ModelDto>[];
    // Models-list errors are per provider; fall back to settings-level error.
    final listErr = widget.models.value?.errors
        .where((e) => e.providerId == p.id)
        .map((e) => e.message)
        .firstOrNull;
    final error = p.enabled ? (listErr ?? p.error) : null;
    final needsKey = p.kind != ProviderKindDto.ollama;

    return Container(
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: error != null ? hk.err : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            onTap: () => setState(() => _expanded = !_expanded),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: HkTokens.s4,
                vertical: HkTokens.s3,
              ),
              child: Row(
                children: [
                  Expanded(
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Wrap(
                          spacing: HkTokens.s2,
                          runSpacing: HkTokens.s1,
                          crossAxisAlignment: WrapCrossAlignment.center,
                          children: [
                            Text(p.id, style: text.titleSmall),
                            _Chip(kindLabel(p.kind), hk.textMuted),
                            if (needsKey && p.hasKey) _Chip('key set', hk.ok),
                            if (needsKey && !p.hasKey) _Chip('no key', hk.warn),
                            if (widget.isDefaultProvider)
                              _Chip('default', hk.accent),
                          ],
                        ),
                        if (p.baseUrl != null && p.baseUrl!.isNotEmpty)
                          Text(
                            p.baseUrl!,
                            style: text.bodySmall?.copyWith(
                              color: hk.textMuted,
                              fontFamily: HkTokens.fontMono,
                            ),
                            overflow: TextOverflow.ellipsis,
                          ),
                      ],
                    ),
                  ),
                  Switch(
                    value: p.enabled,
                    onChanged: _busy
                        ? null
                        : (v) =>
                              _run(() => ctl.saveProvider(_input(enabled: v))),
                  ),
                  Icon(
                    _expanded ? Icons.expand_less : Icons.expand_more,
                    color: hk.textMuted,
                  ),
                ],
              ),
            ),
          ),
          if (error != null)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                HkTokens.s4,
                0,
                HkTokens.s4,
                HkTokens.s3,
              ),
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  Icon(Icons.error_outline, size: 16, color: hk.err),
                  const SizedBox(width: HkTokens.s2),
                  Expanded(
                    child: Text(
                      error,
                      style: text.bodySmall?.copyWith(color: hk.err),
                    ),
                  ),
                ],
              ),
            ),
          if (_expanded) ...[
            Divider(height: 1, color: hk.border),
            Padding(
              padding: const EdgeInsets.all(HkTokens.s4),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (needsKey) ...[
                    Row(
                      children: [
                        Expanded(
                          child: TextField(
                            controller: _key,
                            obscureText: true,
                            enableSuggestions: false,
                            autocorrect: false,
                            decoration: InputDecoration(
                              labelText: p.hasKey
                                  ? 'Replace API key'
                                  : 'API key',
                              border: const OutlineInputBorder(),
                              isDense: true,
                            ),
                          ),
                        ),
                        const SizedBox(width: HkTokens.s2),
                        FilledButton(
                          onPressed: _busy
                              ? null
                              : () async {
                                  final v = _key.text.trim();
                                  if (v.isEmpty) return;
                                  await _run(() => ctl.setApiKey(p.id, v));
                                  if (_actionError == null) _key.clear();
                                },
                          child: const Text('Save key'),
                        ),
                        if (p.hasKey) ...[
                          const SizedBox(width: HkTokens.s2),
                          OutlinedButton(
                            onPressed: _busy
                                ? null
                                : () => _run(() => ctl.setApiKey(p.id, null)),
                            child: const Text('Remove'),
                          ),
                        ],
                      ],
                    ),
                    const SizedBox(height: HkTokens.s3),
                  ],
                  Wrap(
                    spacing: HkTokens.s2,
                    runSpacing: HkTokens.s2,
                    children: [
                      OutlinedButton.icon(
                        icon: const Icon(Icons.sync, size: 18),
                        label: const Text('Test / refresh models'),
                        onPressed: _busy
                            ? null
                            : () => _run(() async {
                                ref.invalidate(modelListProvider);
                                await ref.read(modelListProvider.future);
                                return null;
                              }),
                      ),
                      if (!p.builtin || p.kind == ProviderKindDto.ollama)
                        OutlinedButton.icon(
                          icon: const Icon(Icons.edit_outlined, size: 18),
                          label: const Text('Edit endpoint'),
                          onPressed: () =>
                              showProviderDialog(context, ref, existing: p),
                        ),
                      if (!p.builtin)
                        OutlinedButton.icon(
                          icon: Icon(
                            Icons.delete_outline,
                            size: 18,
                            color: hk.err,
                          ),
                          label: Text(
                            'Remove',
                            style: TextStyle(color: hk.err),
                          ),
                          onPressed: _busy
                              ? null
                              : () => _run(() => ctl.removeProvider(p.id)),
                        ),
                    ],
                  ),
                  if (_busy)
                    const Padding(
                      padding: EdgeInsets.only(top: HkTokens.s2),
                      child: LinearProgressIndicator(),
                    ),
                  if (_actionError != null)
                    Padding(
                      padding: const EdgeInsets.only(top: HkTokens.s2),
                      child: Text(
                        _actionError!,
                        style: text.bodySmall?.copyWith(color: hk.err),
                      ),
                    ),
                  const SizedBox(height: HkTokens.s3),
                  Text(
                    p.enabled
                        ? 'Models (${listed.length})'
                        : 'Enable provider to list models',
                    style: text.labelMedium,
                  ),
                  for (final m in listed) _ModelRow(model: m),
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

class _ModelRow extends ConsumerWidget {
  const _ModelRow({required this.model});
  final ModelDto model;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final s = ref.watch(settingsProvider).value;
    final isDefault = s?.defaultModel == model.reference;
    final ctx = int.tryParse(model.contextWindow.toString()) ?? 0;
    return Padding(
      padding: const EdgeInsets.only(top: HkTokens.s2),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(model.name, style: text.bodyMedium),
                Wrap(
                  spacing: HkTokens.s1,
                  children: [
                    if (ctx > 0)
                      _Chip(
                        '${ctx >= 1000 ? '${(ctx / 1000).round()}k' : ctx} ctx',
                        hk.textMuted,
                      ),
                    if (model.tools) _Chip('tools', hk.info),
                    if (model.reasoning) _Chip('reasoning', hk.accent2),
                    if (model.vision) _Chip('vision', hk.info),
                  ],
                ),
              ],
            ),
          ),
          if (isDefault)
            _Chip('default', hk.accent)
          else if (s != null)
            TextButton(
              onPressed: () => ref
                  .read(settingsProvider.notifier)
                  .setPreferences(
                    PreferencesDto(
                      defaultModel: model.reference,
                      mode: s.mode,
                      workspaceRoot: s.workspaceRoot,
                    ),
                  ),
              child: const Text('Set default'),
            ),
        ],
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip(this.label, this.color);
  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 2),
      decoration: BoxDecoration(
        border: Border.all(color: color.withValues(alpha: 0.6)),
        borderRadius: BorderRadius.circular(HkTokens.radiusChip),
      ),
      child: Text(
        label,
        style: TextStyle(fontSize: 12, color: color, height: 1.3),
      ),
    );
  }
}
