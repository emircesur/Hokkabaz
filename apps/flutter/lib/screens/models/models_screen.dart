import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'provider_card.dart';
import 'provider_dialog.dart';
import 'preferences_section.dart';

/// DESIGN §4 Models tab: providers, models, defaults.
class ModelsScreen extends ConsumerWidget {
  const ModelsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final settings = ref.watch(settingsProvider);
    final hk = context.hk;
    return settings.when(
      loading: () => const Center(child: CircularProgressIndicator()),
      error: (e, _) => Center(
        child: Padding(
          padding: const EdgeInsets.all(HkTokens.s4),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.error_outline, color: hk.err, size: 32),
              const SizedBox(height: HkTokens.s2),
              Text('$e', textAlign: TextAlign.center),
              const SizedBox(height: HkTokens.s3),
              OutlinedButton(
                onPressed: () => ref.invalidate(settingsProvider),
                child: const Text('Retry'),
              ),
            ],
          ),
        ),
      ),
      data: (s) => _Body(settings: s),
    );
  }
}

class _Body extends ConsumerWidget {
  const _Body({required this.settings});
  final SettingsDto settings;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final models = ref.watch(modelListProvider);
    return Align(
      alignment: Alignment.topCenter,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 800),
        child: ListView(
          padding: const EdgeInsets.all(HkTokens.s4),
          children: [
            Row(
              children: [
                Expanded(child: Text('Models', style: text.titleLarge)),
                OutlinedButton.icon(
                  icon: const Icon(Icons.refresh, size: 18),
                  label: const Text('Refresh catalog'),
                  onPressed: () async {
                    final err = await ref
                        .read(settingsProvider.notifier)
                        .refreshCatalog();
                    if (context.mounted) {
                      _toast(context, err ?? 'Catalog refreshed');
                    }
                  },
                ),
              ],
            ),
            const SizedBox(height: HkTokens.s4),
            PreferencesSection(settings: settings, models: models),
            const SizedBox(height: HkTokens.s6),
            Row(
              children: [
                Expanded(child: Text('Providers', style: text.titleMedium)),
                FilledButton.icon(
                  icon: const Icon(Icons.add, size: 18),
                  label: const Text('Add endpoint'),
                  onPressed: () => showProviderDialog(context, ref),
                ),
              ],
            ),
            const SizedBox(height: HkTokens.s2),
            for (final p in settings.providers)
              Padding(
                padding: const EdgeInsets.only(bottom: HkTokens.s2),
                child: ProviderCard(
                  provider: p,
                  models: models,
                  isDefaultProvider:
                      settings.defaultModel?.startsWith('${p.id}/') ?? false,
                ),
              ),
            const SizedBox(height: HkTokens.s4),
            const _OllamaPlaceholder(),
          ],
        ),
      ),
    );
  }
}

void _toast(BuildContext context, String msg) {
  ScaffoldMessenger.of(context)
    ..hideCurrentSnackBar()
    ..showSnackBar(SnackBar(content: Text(msg)));
}

/// Local pull / routing UI lands with P6.8.
class _OllamaPlaceholder extends StatelessWidget {
  const _OllamaPlaceholder();

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    return Container(
      padding: const EdgeInsets.all(HkTokens.s4),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Row(
        children: [
          Icon(Icons.download_for_offline_outlined, color: hk.textMuted),
          const SizedBox(width: HkTokens.s3),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text('Local models (Ollama)', style: text.titleSmall),
                Text(
                  'Model pull and routing are not available yet.',
                  style: text.bodySmall?.copyWith(color: hk.textMuted),
                ),
              ],
            ),
          ),
        ],
      ),
    );
  }
}
