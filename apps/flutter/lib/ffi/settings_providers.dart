import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../src/rust/api/settings.dart' as api;
import '../src/rust/dto.dart';
import 'ffi.dart' show rustInitErrorProvider;

/// Settings snapshot plus the model list. Every mutation returns fresh
/// settings from Rust, so the notifier just swaps state; errors surface as
/// [AsyncError] and never throw into widgets.
class SettingsController extends AsyncNotifier<SettingsDto> {
  @override
  Future<SettingsDto> build() async {
    final initError = ref.watch(rustInitErrorProvider);
    if (initError != null) throw StateError('Rust unavailable: $initError');
    return api.getSettings();
  }

  /// Runs [op], stores the returned settings, and refreshes models since any
  /// provider change can alter them. Returns an error string or null.
  Future<String?> _apply(Future<SettingsDto> Function() op) async {
    try {
      final next = await op();
      state = AsyncData(next);
      ref.invalidate(modelListProvider);
      return null;
    } catch (e) {
      return e.toString();
    }
  }

  Future<String?> saveProvider(ProviderInputDto input) =>
      _apply(() => api.saveProvider(input: input));

  Future<String?> removeProvider(String id) =>
      _apply(() => api.removeProvider(providerId: id));

  /// Write-only: null/empty deletes the key. The key is never read back.
  Future<String?> setApiKey(String id, String? key) =>
      _apply(() => api.setApiKey(providerId: id, key: key));

  Future<String?> setPreferences(PreferencesDto prefs) =>
      _apply(() => api.setPreferences(prefs: prefs));

  /// Re-fetches models.dev, then reloads settings and the model list.
  Future<String?> refreshCatalog() async {
    try {
      await api.refreshCatalog();
      state = AsyncData(await api.getSettings());
      ref.invalidate(modelListProvider);
      return null;
    } catch (e) {
      return e.toString();
    }
  }
}

final settingsProvider = AsyncNotifierProvider<SettingsController, SettingsDto>(
  SettingsController.new,
);

/// Models of enabled providers; per-provider failures are in `errors`.
final modelListProvider = FutureProvider<ModelListDto>((ref) async {
  if (ref.watch(rustInitErrorProvider) != null) {
    return const ModelListDto(models: [], errors: []);
  }
  return api.listModels();
});
