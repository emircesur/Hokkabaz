import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/adaptive.dart';
import '../../app/breakpoints.dart';
import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';
import 'composer_logic.dart';

/// Searchable model list from [modelListProvider]; writes [composerModelProvider].
Future<void> showModelPicker(BuildContext context) {
  return showAdaptiveSheetOrDialog<void>(
    context,
    maxHeight: 520,
    tall: true,
    builder: (_) => const _ModelPicker(),
  );
}

class _ModelPicker extends ConsumerStatefulWidget {
  const _ModelPicker();

  @override
  ConsumerState<_ModelPicker> createState() => _ModelPickerState();
}

class _ModelPickerState extends ConsumerState<_ModelPicker> {
  String _q = '';

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final list = ref.watch(modelListProvider);
    final current = ref.watch(composerModelProvider);
    return Padding(
      padding: const EdgeInsets.all(HkTokens.s3),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          TextField(
            autofocus: true,
            decoration: InputDecoration(
              isDense: !isNarrow(context),
              prefixIcon: const Icon(Icons.search),
              hintText: 'Search models',
              border: const OutlineInputBorder(),
            ),
            onChanged: (v) => setState(() => _q = v),
          ),
          const SizedBox(height: HkTokens.s2),
          Flexible(
            child: list.when(
              loading: () => const Padding(
                padding: EdgeInsets.all(HkTokens.s4),
                child: Center(child: CircularProgressIndicator()),
              ),
              error: (e, _) =>
                  Text(describeError(e), style: TextStyle(color: hk.err)),
              data: (d) => _body(context, d.models, current),
            ),
          ),
        ],
      ),
    );
  }

  Widget _body(BuildContext context, List<ModelDto> models, String? current) {
    final hk = context.hk;
    final byRef = {for (final m in models) m.reference: m};
    final refs = fuzzyFilter(_q, [...byRef.keys], limit: 100);
    void pick(String? r) {
      ref.read(composerModelProvider.notifier).set(r);
      Navigator.of(context).pop();
    }

    return ListView(
      shrinkWrap: true,
      children: [
        if (_q.isEmpty)
          ListTile(
            dense: !isNarrow(context),
            selected: current == null,
            leading: const Icon(Icons.auto_awesome_outlined, size: 18),
            title: const Text('Session default'),
            onTap: () => pick(null),
          ),
        for (final r in refs)
          ListTile(
            dense: !isNarrow(context),
            selected: r == current,
            title: Text(byRef[r]!.name, overflow: TextOverflow.ellipsis),
            subtitle: Text(
              r,
              overflow: TextOverflow.ellipsis,
              style: TextStyle(fontSize: 11, color: hk.textMuted),
            ),
            onTap: () => pick(r),
          ),
        if (refs.isEmpty && models.isEmpty)
          Padding(
            padding: const EdgeInsets.all(HkTokens.s3),
            child: Text(
              'No models. Enable a provider in Models.',
              style: TextStyle(color: hk.textMuted),
            ),
          ),
      ],
    );
  }
}
