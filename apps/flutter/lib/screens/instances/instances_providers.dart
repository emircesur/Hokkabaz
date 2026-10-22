import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../ffi/settings_providers.dart';
import '../../src/rust/api/instances.dart' as api;
import '../../src/rust/dto.dart';
import '../chat/chat_providers.dart'
    show describeError, selectedSessionProvider, sessionListProvider;

/// "This device" plus remote instances. Every action returns an error string
/// (null = ok) and reloads the list, so widgets never see a thrown error.
class InstanceBook extends AsyncNotifier<List<InstanceDto>> {
  @override
  Future<List<InstanceDto>> build() async {
    if (ref.watch(backendErrorProvider) != null) return const [];
    return api.listInstances();
  }

  Future<String?> _run(Future<void> Function() op) async {
    String? err;
    try {
      await op();
    } catch (e) {
      err = describeError(e);
    }
    // Reload even on failure: the book may have changed before the error.
    state = await AsyncValue.guard(api.listInstances);
    return err;
  }

  Future<String?> add(String name, String url, String token) => _run(
    () => api.addInstance(name: name, baseUrl: url, token: token),
  );

  Future<String?> pair(String uri, String? name) => _run(
    () => api.pairFromUri(
      uri: uri,
      name: (name == null || name.trim().isEmpty) ? null : name.trim(),
    ),
  );

  Future<String?> remove(String id) async {
    final wasConnected = state.value?.any((i) => i.id == id && i.connected);
    final err = await _run(() => api.removeInstance(id: id));
    if (err == null && wasConnected == true) _resetBackendState();
    return err;
  }

  Future<String?> setDefault(String id) =>
      _run(() => api.selectInstance(id: id));

  /// Hot-swaps the active core. Chat and model state belongs to the old core,
  /// so it is dropped on success.
  Future<String?> connect(String id) async {
    final err = await _run(() => api.connectInstance(id: id));
    if (err == null) _resetBackendState();
    return err;
  }

  void _resetBackendState() {
    ref.read(selectedSessionProvider.notifier).select(null);
    ref.invalidate(sessionListProvider);
    ref.invalidate(settingsProvider);
    ref.invalidate(modelListProvider);
  }
}

final instanceBookProvider =
    AsyncNotifierProvider<InstanceBook, List<InstanceDto>>(InstanceBook.new);

/// Health + auth probe of one instance. Invalidate to re-probe.
final instanceStatusProvider = FutureProvider.autoDispose
    .family<InstanceStatusDto, String>((ref, id) => api.instanceStatus(id: id));
