import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/theme_mode.dart';
import '../../app/version.dart';
import '../../ffi/android_runtime.dart';
import '../../ffi/ffi.dart' show rustInitErrorProvider;
import '../../ffi/settings_providers.dart';
import '../../theme/tokens.dart';
import 'updates.dart';

/// App-level settings: appearance, updates, device info, about.
class SettingsScreen extends ConsumerWidget {
  const SettingsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final hk = context.hk;
    final theme = ref.watch(themeModeProvider);
    final update = ref.watch(updateCheckProvider);
    final checked = ref.read(updateCheckProvider.notifier).checked;
    final core = ref.watch(rustInitErrorProvider) == null
        ? ref.watch(settingsProvider).value
        : null;
    return Align(
      alignment: Alignment.topCenter,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 720),
        child: ListView(
          padding: const EdgeInsets.all(HkTokens.s4),
          children: [
            Text('Settings', style: text.titleLarge),
            const SizedBox(height: HkTokens.s4),
            Text('Appearance', style: text.titleSmall),
            const SizedBox(height: HkTokens.s2),
            SegmentedButton<ThemeMode>(
              segments: [
                for (final m in ThemeMode.values)
                  ButtonSegment(
                    value: m,
                    label: Text(themeModeLabel(m)),
                    icon: Icon(themeModeIcon(m), size: 18),
                  ),
              ],
              selected: {theme},
              onSelectionChanged: (s) =>
                  ref.read(themeModeProvider.notifier).set(s.first),
            ),
            const SizedBox(height: HkTokens.s6),
            Text('Text size', style: text.titleSmall),
            const SizedBox(height: HkTokens.s2),
            Consumer(
              builder: (context, ref, _) {
                final v = ref.watch(uiScaleProvider);
                return Row(
                  children: [
                    const Icon(Icons.text_decrease, size: 18),
                    Expanded(
                      child: Slider(
                        value: v,
                        min: 0.85,
                        max: 1.4,
                        divisions: 11,
                        label: '${(v * 100).round()}%',
                        onChanged: ref.read(uiScaleProvider.notifier).set,
                      ),
                    ),
                    const Icon(Icons.text_increase, size: 18),
                    const SizedBox(width: HkTokens.s2),
                    Text('${(v * 100).round()}%'),
                  ],
                );
              },
            ),
            const SizedBox(height: HkTokens.s4),
            Text('Updates', style: text.titleSmall),
            const SizedBox(height: HkTokens.s1),
            Text(
              'Version $hkVersion',
              style: text.bodySmall?.copyWith(color: hk.textMuted),
            ),
            const SizedBox(height: HkTokens.s2),
            Row(
              children: [
                OutlinedButton.icon(
                  icon: update.isLoading
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Icon(Icons.system_update_alt, size: 18),
                  label: const Text('Check for updates'),
                  onPressed: update.isLoading
                      ? null
                      : () => ref.read(updateCheckProvider.notifier).run(),
                ),
              ],
            ),
            const SizedBox(height: HkTokens.s2),
            update.when(
              loading: () => const SizedBox.shrink(),
              error: (e, _) =>
                  Text('Could not check: $e', style: TextStyle(color: hk.err)),
              data: (r) => r != null
                  ? Card(
                      child: ListTile(
                        leading: Icon(
                          Icons.new_releases_outlined,
                          color: hk.accent,
                        ),
                        title: Text('${r.tag} is available'),
                        subtitle: Text(
                          r.notes.isEmpty ? r.url : r.notes,
                          maxLines: 4,
                          overflow: TextOverflow.ellipsis,
                        ),
                        trailing: TextButton(
                          onPressed: () async {
                            await Clipboard.setData(ClipboardData(text: r.url));
                            if (context.mounted) {
                              ScaffoldMessenger.of(context).showSnackBar(
                                const SnackBar(
                                  content: Text('Release link copied'),
                                ),
                              );
                            }
                          },
                          child: const Text('Copy link'),
                        ),
                      ),
                    )
                  : Text(
                      checked
                          ? 'You are on the latest version.'
                          : 'Updates are never installed automatically.',
                      style: text.bodySmall,
                    ),
            ),
            if (core != null) ...[
              const SizedBox(height: HkTokens.s6),
              Text('This device', style: text.titleSmall),
              const SizedBox(height: HkTokens.s2),
              _kv('Data folder', core.dataDir, context),
              _kv('Workspace', core.workspaceRoot, context),
            ],
            if (Platform.isAndroid) ...[
              const SizedBox(height: HkTokens.s6),
              Text('Background work', style: text.titleSmall),
              const SizedBox(height: HkTokens.s1),
              Text(
                'While a chat turn is running Hokkabaz shows a notification and keeps the app alive. '
                'Battery saver can still pause it.',
                style: text.bodySmall?.copyWith(color: hk.textMuted),
              ),
              const SizedBox(height: HkTokens.s2),
              OutlinedButton.icon(
                icon: const Icon(Icons.notifications_outlined, size: 18),
                label: const Text('Allow notifications'),
                onPressed: requestNotificationPermission,
              ),
            ],
            const SizedBox(height: HkTokens.s6),
            Text('About', style: text.titleSmall),
            const SizedBox(height: HkTokens.s1),
            Text(
              'Hokkabaz is free software under the GNU AGPL-3.0-or-later. Source: https://github.com/$hkRepo',
              style: text.bodySmall?.copyWith(color: hk.textMuted),
            ),
            const SizedBox(height: HkTokens.s2),
            TextButton(
              onPressed: () => showLicensePage(
                context: context,
                applicationName: 'Hokkabaz',
                applicationVersion: hkVersion,
              ),
              child: const Text('Open-source licenses'),
            ),
          ],
        ),
      ),
    );
  }

  Widget _kv(String k, String v, BuildContext context) {
    final hk = context.hk;
    return Padding(
      padding: const EdgeInsets.only(bottom: HkTokens.s1),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 110,
            child: Text(k, style: TextStyle(color: hk.textMuted)),
          ),
          Expanded(child: SelectableText(v)),
        ],
      ),
    );
  }
}
