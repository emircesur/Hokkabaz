import 'dart:convert';

Map<String, dynamic> _m(Object? v) => (v as Map).cast<String, dynamic>();
List<String> _strs(Object? v) =>
    (v as List? ?? const []).map((e) => '$e').toList();

class PluginInfo {
  PluginInfo.fromJson(Map<String, dynamic> j)
    : id = j['id'] as String,
      name = j['name'] as String? ?? j['id'] as String,
      version = j['version'] as String? ?? '',
      description = j['description'] as String? ?? '',
      author = j['author'] as String? ?? '',
      enabled = j['enabled'] as bool? ?? false,
      loaded = j['loaded'] as bool? ?? false,
      capabilities = _strs(j['capabilities']),
      error = j['error'] as String?,
      tools = _strs(j['tools']),
      nodes = _strs(j['nodes']),
      triggers = _strs(j['triggers']),
      settingsSchema = j['settings_schema'] is Map
          ? _m(j['settings_schema'])
          : null,
      settings = j['settings'] is Map ? _m(j['settings']) : const {};

  final String id, name, version, description, author;
  final bool enabled, loaded;
  final List<String> capabilities, tools, nodes, triggers;
  final String? error;
  final Map<String, dynamic>? settingsSchema;
  final Map<String, dynamic> settings;

  static List<PluginInfo> parseList(String raw) =>
      (jsonDecode(raw) as List).map((e) => PluginInfo.fromJson(_m(e))).toList();
}

class RegistryEntry {
  RegistryEntry.fromJson(Map<String, dynamic> j)
    : id = j['id'] as String,
      name = j['name'] as String? ?? j['id'] as String,
      version = j['version'] as String? ?? '',
      description = j['description'] as String? ?? '',
      installedVersion = j['installed_version'] as String?,
      updateAvailable = j['update_available'] as bool? ?? false,
      capabilities = _strs(j['capability_summary']);

  final String id, name, version, description;
  final String? installedVersion;
  final bool updateAvailable;
  final List<String> capabilities;

  static List<RegistryEntry> parse(String raw) =>
      (_m(jsonDecode(raw))['plugins'] as List)
          .map((e) => RegistryEntry.fromJson(_m(e)))
          .toList();
}

class McpServer {
  McpServer.fromJson(Map<String, dynamic> j)
    : name = j['name'] as String,
      transport = j['transport'] as String? ?? 'http',
      target = j['target'] as String? ?? '',
      trusted = j['trusted'] as bool? ?? false,
      enabled = j['enabled'] as bool? ?? true,
      connected = j['connected'] as bool? ?? false,
      tools = _strs(j['tools']),
      error = j['error'] as String?,
      stdioSupported = j['stdio_supported'] as bool? ?? true;

  final String name, transport, target;
  final bool trusted, enabled, connected, stdioSupported;
  final List<String> tools;
  final String? error;

  static List<McpServer> parseList(String raw) =>
      (jsonDecode(raw) as List).map((e) => McpServer.fromJson(_m(e))).toList();
}

class CredField {
  CredField.fromJson(Map<String, dynamic> j)
    : name = j['name'] as String,
      label = j['display_name'] as String? ?? j['name'] as String,
      type = j['type'] as String? ?? 'string',
      required = j['required'] as bool? ?? false,
      secret = j['secret'] as bool? ?? false,
      description = j['description'] as String?,
      defaultValue = j['default'],
      options = (j['options'] as List? ?? const [])
          .whereType<Map>()
          .map((o) => (o['value'], '${o['name'] ?? o['value']}'))
          .toList();

  final String name, label, type;
  final bool required, secret;
  final String? description;
  final Object? defaultValue;
  final List<(Object?, String)> options;

  /// Fields the form can edit as text; the rest (notices, collections) are skipped.
  bool get editable => const {
    'string',
    'number',
    'boolean',
    'options',
  }.contains(type);
}

class CredType {
  CredType.fromJson(Map<String, dynamic> j)
    : name = j['name'] as String,
      displayName = j['display_name'] as String? ?? j['name'] as String,
      docsUrl = j['documentation_url'] as String?,
      fields = (j['fields'] as List)
          .map((f) => CredField.fromJson(_m(f)))
          .where((f) => f.editable)
          .toList();

  final String name, displayName;
  final String? docsUrl;
  final List<CredField> fields;

  static List<CredType> parseList(String raw) =>
      (jsonDecode(raw) as List).map((e) => CredType.fromJson(_m(e))).toList();
}

class CredInfo {
  CredInfo.fromJson(Map<String, dynamic> j)
    : id = j['id'] as String,
      name = j['name'] as String,
      type = j['type'] as String;

  final String id, name, type;

  static List<CredInfo> parseList(String raw) =>
      (jsonDecode(raw) as List).map((e) => CredInfo.fromJson(_m(e))).toList();
}
