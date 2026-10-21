import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';

/// Default model, default mode, workspace root.
class PreferencesSection extends ConsumerStatefulWidget {
  const PreferencesSection({
    super.key,
    required this.settings,
    required this.models,
  });

  final SettingsDto settings;
  final AsyncValue<ModelListDto> models;

  @override
  ConsumerState<PreferencesSection> createState() => _PreferencesSectionState();
}

class _PreferencesSectionState extends ConsumerState<PreferencesSection> {
  late final TextEditingController _root;
  String? _error;

  @override
  void initState() {
    super.initState();
    _root = TextEditingController(text: widget.settings.workspaceRoot);
  }

  @override
  void didUpdateWidget(PreferencesSection old) {
    super.didUpdateWidget(old);
    if (old.settings.workspaceRoot != widget.settings.workspaceRoot &&
        _root.text != widget.settings.workspaceRoot) {
      _root.text = widget.settings.workspaceRoot;
    }
  }

  @override
  void dispose() {
    _root.dispose();
    super.dispose();
  }

  Future<void> _save({
    Object? model = _keep,
    ModeDto? mode,
    Object? root = _keep,
  }) async {
    final s = widget.settings;
    final err = await ref
        .read(settingsProvider.notifier)
        .setPreferences(
          PreferencesDto(
            defaultModel: identical(model, _keep)
                ? s.defaultModel
                : model as String?,
            mode: mode ?? s.mode,
            workspaceRoot: identical(root, _keep)
                ? s.workspaceRoot
                : root as String?,
          ),
        );
    if (mounted) setState(() => _error = err);
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final s = widget.settings;
    final refs = <String>{
      ...?widget.models.value?.models.map((m) => m.reference),
      ?s.defaultModel,
    }.toList()..sort();
    final names = {
      for (final m in widget.models.value?.models ?? <ModelDto>[])
        m.reference: m.name,
    };

    return Container(
      padding: const EdgeInsets.all(HkTokens.s4),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text('Defaults', style: text.titleMedium),
          const SizedBox(height: HkTokens.s3),
          DropdownButtonFormField<String?>(
            key: ValueKey('default-model-${s.defaultModel}-${refs.length}'),
            initialValue: s.defaultModel,
            isExpanded: true,
            decoration: const InputDecoration(
              labelText: 'Default model',
              border: OutlineInputBorder(),
              isDense: true,
            ),
            items: [
              const DropdownMenuItem<String?>(value: null, child: Text('None')),
              for (final r in refs)
                DropdownMenuItem<String?>(
                  value: r,
                  child: Text(
                    names[r] == null ? r : '${names[r]}  ($r)',
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
            ],
            onChanged: (v) => _save(model: v),
          ),
          if (widget.models.isLoading)
            const Padding(
              padding: EdgeInsets.only(top: HkTokens.s2),
              child: LinearProgressIndicator(),
            ),
          const SizedBox(height: HkTokens.s3),
          Text('Default mode', style: text.labelMedium),
          const SizedBox(height: HkTokens.s1),
          SegmentedButton<ModeDto>(
            segments: const [
              ButtonSegment(
                value: ModeDto.build,
                label: Text('Build'),
                icon: Icon(Icons.build_outlined, size: 16),
              ),
              ButtonSegment(
                value: ModeDto.plan,
                label: Text('Plan'),
                icon: Icon(Icons.visibility_outlined, size: 16),
              ),
            ],
            selected: {s.mode},
            onSelectionChanged: (v) => _save(mode: v.first),
          ),
          const SizedBox(height: HkTokens.s3),
          TextField(
            controller: _root,
            decoration: InputDecoration(
              labelText: 'Workspace root',
              helperText: 'Empty resets to the default workspace',
              border: const OutlineInputBorder(),
              isDense: true,
              suffixIcon: IconButton(
                tooltip: 'Save',
                icon: const Icon(Icons.check),
                onPressed: () {
                  final v = _root.text.trim();
                  _save(root: v.isEmpty ? null : v);
                },
              ),
            ),
            onSubmitted: (v) => _save(root: v.trim().isEmpty ? null : v.trim()),
          ),
          if (_error != null)
            Padding(
              padding: const EdgeInsets.only(top: HkTokens.s2),
              child: Text(
                _error!,
                style: text.bodySmall?.copyWith(color: hk.err),
              ),
            ),
        ],
      ),
    );
  }
}

const Object _keep = Object();
