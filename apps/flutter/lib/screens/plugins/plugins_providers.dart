import 'dart:convert';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart' show rustInitErrorProvider;
import '../../src/rust/api/credentials.dart' as cred;
import '../../src/rust/api/plugins.dart' as api;
import '../chat/chat_providers.dart' show describeError;
import 'plugins_models.dart';

/// Every mutation returns an error string (null = ok) and reloads, so widgets never see a throw.
Future<String?> _guard(
  Future<void> Function() op,
  void Function() after,
) async {
  String? err;
  try {
    await op();
  } catch (e) {
    err = describeError(e);
  }
  after();
  return err;
}

class PluginBook extends AsyncNotifier<List<PluginInfo>> {
  @override
  Future<List<PluginInfo>> build() async {
    if (ref.watch(rustInitErrorProvider) != null) return const [];
    return PluginInfo.parseList(await api.pluginList());
  }

  Future<void> _reload() async => state = await AsyncValue.guard(
    () async => PluginInfo.parseList(await api.pluginList()),
  );

  Future<String?> installFile(String path) =>
      _guard(() => api.pluginInstallFile(path: path), _reload);
  Future<String?> installFromRegistry(String id, {String? indexUrl}) => _guard(
    () => api.pluginInstallFromRegistry(id: id, indexUrl: indexUrl),
    () {
      _reload();
      ref.invalidate(registryProvider);
    },
  );
  Future<String?> remove(String id) =>
      _guard(() => api.pluginRemove(id: id), _reload);
  Future<String?> setEnabled(String id, bool on) =>
      _guard(() => api.pluginSetEnabled(id: id, enabled: on), _reload);
  Future<String?> setSettings(String id, Map<String, dynamic> s) => _guard(
    () => api.pluginSetSettings(id: id, settingsJson: jsonEncode(s)),
    _reload,
  );
}

final pluginBookProvider = AsyncNotifierProvider<PluginBook, List<PluginInfo>>(
  PluginBook.new,
);

class RegistryUrl extends Notifier<String?> {
  @override
  String? build() => null;
  void set(String? v) =>
      state = (v == null || v.trim().isEmpty) ? null : v.trim();
}

final registryUrlProvider = NotifierProvider<RegistryUrl, String?>(
  RegistryUrl.new,
);

final registryProvider = FutureProvider.autoDispose<List<RegistryEntry>>((
  ref,
) async {
  final url = ref.watch(registryUrlProvider);
  return RegistryEntry.parse(await api.pluginRegistry(indexUrl: url));
});

class McpBook extends AsyncNotifier<List<McpServer>> {
  @override
  Future<List<McpServer>> build() async {
    if (ref.watch(rustInitErrorProvider) != null) return const [];
    return McpServer.parseList(await api.mcpList());
  }

  Future<void> _reload() async => state = await AsyncValue.guard(
    () async => McpServer.parseList(await api.mcpList()),
  );

  Future<String?> save(Map<String, dynamic> spec) =>
      _guard(() => api.mcpSave(specJson: jsonEncode(spec)), _reload);
  Future<String?> setTrusted(String name, bool t) =>
      _guard(() => api.mcpSetTrusted(name: name, trusted: t), _reload);
  Future<String?> setEnabled(String name, bool on) =>
      _guard(() => api.mcpSetEnabled(name: name, enabled: on), _reload);
  Future<String?> remove(String name) =>
      _guard(() => api.mcpRemove(name: name), _reload);
  Future<String?> connect(String name) =>
      _guard(() => api.mcpConnect(name: name), _reload);
}

final mcpBookProvider = AsyncNotifierProvider<McpBook, List<McpServer>>(
  McpBook.new,
);

class CredentialBook extends AsyncNotifier<List<CredInfo>> {
  @override
  Future<List<CredInfo>> build() async {
    if (ref.watch(rustInitErrorProvider) != null) return const [];
    return CredInfo.parseList(await cred.credentialList());
  }

  Future<void> _reload() async => state = await AsyncValue.guard(
    () async => CredInfo.parseList(await cred.credentialList()),
  );

  Future<({String? error, CredInfo? info})> create(
    String name,
    String kind,
    Map<String, dynamic> data,
  ) async {
    CredInfo? info;
    final err = await _guard(() async {
      final raw = await cred.credentialCreate(
        name: name,
        kind: kind,
        dataJson: jsonEncode(data),
      );
      info = CredInfo.fromJson((jsonDecode(raw) as Map).cast());
    }, _reload);
    return (error: err, info: info);
  }

  Future<String?> edit(String id, {String? name, Map<String, dynamic>? data}) =>
      _guard(
        () => cred.credentialUpdate(
          id: id,
          name: name,
          dataJson: data == null ? null : jsonEncode(data),
        ),
        _reload,
      );
  Future<String?> remove(String id) =>
      _guard(() => cred.credentialDelete(id: id), _reload);
}

final credentialBookProvider =
    AsyncNotifierProvider<CredentialBook, List<CredInfo>>(CredentialBook.new);

final credentialTypesProvider = FutureProvider<List<CredType>>(
  (ref) async => CredType.parseList(await cred.credentialTypes()),
);

/// Which fields of a stored credential have a value (never the values).
final credentialSetFieldsProvider = FutureProvider.autoDispose
    .family<List<String>, String>(
      (ref, id) => cred.credentialSetFields(id: id),
    );
