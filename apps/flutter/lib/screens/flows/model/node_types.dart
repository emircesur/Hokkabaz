import 'dart:convert';

import 'workflow_model.dart';

/// Node type catalog entry (mirrors Rust `NodeTypeDto`, kept FFI-free so the
/// model stays testable). [properties] come from the n8n description JSON.
class NodeTypeInfo {
  const NodeTypeInfo({
    required this.typeName,
    required this.versions,
    required this.defaultVersion,
    required this.displayName,
    required this.description,
    required this.group,
    required this.inputs,
    required this.outputs,
    this.inputsExpr,
    this.outputsExpr,
    this.outputNames = const [],
    this.properties = const [],
  });

  factory NodeTypeInfo.fromFields({
    required String typeName,
    required List<double> versions,
    required double defaultVersion,
    required String displayName,
    required String description,
    required List<String> group,
    required List<String> inputs,
    required List<String> outputs,
    String? inputsExpr,
    String? outputsExpr,
    List<String> outputNames = const [],
    required String propertiesJson,
  }) {
    var props = <ParamSpec>[];
    try {
      final j = jsonDecode(propertiesJson);
      if (j is List) {
        props = [
          for (final p in j)
            if (p is Map<String, dynamic>) ParamSpec.fromJson(p),
        ];
      }
    } on FormatException {
      // Bad description JSON: node stays usable with the raw parameter editor.
    }
    return NodeTypeInfo(
      typeName: typeName,
      versions: versions,
      defaultVersion: defaultVersion,
      displayName: displayName,
      description: description,
      group: group,
      inputs: inputs,
      outputs: outputs,
      inputsExpr: inputsExpr,
      outputsExpr: outputsExpr,
      outputNames: outputNames,
      properties: props,
    );
  }

  final String typeName;
  final List<double> versions;
  final double defaultVersion;
  final String displayName;
  final String description;
  final List<String> group;
  final List<String> inputs;
  final List<String> outputs;
  final String? inputsExpr;
  final String? outputsExpr;
  final List<String> outputNames;
  final List<ParamSpec> properties;

  bool get isTrigger => group.contains('trigger');
  NodeCategory get category => categoryOf(typeName, group: group);

  bool supportsVersion(num v) => versions.contains(v.toDouble());
}

enum NodeCategory {
  trigger('Trigger'),
  ai('AI'),
  logic('Logic'),
  data('Data'),
  integration('Integration');

  const NodeCategory(this.label);
  final String label;
}

const _logicTypes = {
  'if',
  'switch',
  'filter',
  'merge',
  'splitInBatches',
  'noOp',
  'wait',
  'executeWorkflow',
  'respondToWebhook',
  'stopAndError',
};

const _dataTypes = {
  'set',
  'code',
  'dateTime',
  'aggregate',
  'splitOut',
  'sort',
  'limit',
  'removeDuplicates',
  'summarize',
  'itemLists',
};

/// Category from the type string (works for unsupported types too).
NodeCategory categoryOf(String type, {List<String> group = const []}) {
  final short = shortType(type);
  if (group.contains('trigger') ||
      short.endsWith('Trigger') ||
      short == 'webhook') {
    return NodeCategory.trigger;
  }
  if (type.contains('langchain') || short.toLowerCase().contains('agent')) {
    return NodeCategory.ai;
  }
  if (_logicTypes.contains(short)) return NodeCategory.logic;
  if (_dataTypes.contains(short)) return NodeCategory.data;
  return NodeCategory.integration;
}

/// `n8n-nodes-base.httpRequest` -> `httpRequest`.
String shortType(String type) {
  final i = type.lastIndexOf('.');
  return i < 0 ? type : type.substring(i + 1);
}

/// One option of an `options` / `multiOptions` param.
class ParamOption {
  const ParamOption(this.name, this.value, [this.description]);
  final String name;
  final Object? value;
  final String? description;
}

/// A named group of a `fixedCollection`.
class ParamGroup {
  const ParamGroup(this.name, this.displayName, this.values);
  final String name;
  final String displayName;
  final List<ParamSpec> values;
}

/// n8n `NodeProperty`, parsed for form generation. Unknown widget types keep
/// [type] as the raw string and fall back to a JSON editor.
class ParamSpec {
  const ParamSpec({
    required this.name,
    required this.displayName,
    required this.type,
    this.defaultValue,
    this.description,
    this.hint,
    this.placeholder,
    this.required = false,
    this.options = const [],
    this.children = const [],
    this.groups = const [],
    this.typeOptions = const {},
    this.show,
    this.hide,
  });

  factory ParamSpec.fromJson(Map<String, dynamic> j) {
    final type = (j['type'] as String?) ?? 'string';
    final rawOpts = j['options'];
    final opts = <ParamOption>[];
    final children = <ParamSpec>[];
    final groups = <ParamGroup>[];
    if (rawOpts is List) {
      for (final o in rawOpts) {
        if (o is! Map<String, dynamic>) continue;
        if (type == 'collection') {
          children.add(ParamSpec.fromJson(o));
        } else if (type == 'fixedCollection') {
          final vals = o['values'];
          groups.add(
            ParamGroup(
              (o['name'] as String?) ?? '',
              (o['displayName'] as String?) ?? (o['name'] as String?) ?? '',
              [
                if (vals is List)
                  for (final v in vals)
                    if (v is Map<String, dynamic>) ParamSpec.fromJson(v),
              ],
            ),
          );
        } else {
          opts.add(
            ParamOption(
              (o['name'] as String?) ?? '${o['value']}',
              o['value'],
              o['description'] as String?,
            ),
          );
        }
      }
    }
    final d = j['displayOptions'];
    return ParamSpec(
      name: (j['name'] as String?) ?? '',
      displayName: (j['displayName'] as String?) ?? (j['name'] as String?) ?? '',
      type: type,
      defaultValue: j['default'],
      description: j['description'] as String?,
      hint: j['hint'] as String?,
      placeholder: j['placeholder'] as String?,
      required: j['required'] == true,
      options: opts,
      children: children,
      groups: groups,
      typeOptions: (j['typeOptions'] is Map<String, dynamic>)
          ? j['typeOptions'] as Map<String, dynamic>
          : const {},
      show: d is Map && d['show'] is Map
          ? Map<String, dynamic>.from(d['show'] as Map)
          : null,
      hide: d is Map && d['hide'] is Map
          ? Map<String, dynamic>.from(d['hide'] as Map)
          : null,
    );
  }

  final String name;
  final String displayName;
  final String type;
  final Object? defaultValue;
  final String? description;
  final String? hint;
  final String? placeholder;
  final bool required;
  final List<ParamOption> options;
  final List<ParamSpec> children;
  final List<ParamGroup> groups;
  final Map<String, dynamic> typeOptions;
  final Map<String, dynamic>? show;
  final Map<String, dynamic>? hide;

  bool get multipleValues => typeOptions['multipleValues'] == true;
  int get rows => (typeOptions['rows'] as num?)?.toInt() ?? 1;

  /// n8n `displayOptions` semantics: every `show` key must match, no `hide` key may.
  bool isVisible(Map<String, dynamic> params, {num typeVersion = 1}) {
    final s = show;
    if (s != null && !s.entries.every((e) => _matches(e, params, typeVersion))) {
      return false;
    }
    final h = hide;
    if (h != null && h.entries.any((e) => _matches(e, params, typeVersion))) {
      return false;
    }
    return true;
  }

  static bool _matches(
    MapEntry<String, dynamic> cond,
    Map<String, dynamic> params,
    num typeVersion,
  ) {
    var key = cond.key;
    final Object? actual;
    if (key == '@version') {
      actual = typeVersion;
    } else {
      if (key.startsWith('/')) key = key.substring(1);
      actual = params[key];
    }
    final wanted = cond.value;
    if (wanted is! List) return false;
    return wanted.any((w) => _matchOne(w, actual));
  }

  static bool _matchOne(Object? want, Object? actual) {
    if (want is Map && want['_cnd'] is Map) {
      final c = (want['_cnd'] as Map).entries.first;
      final v = c.value;
      switch (c.key) {
        case 'eq':
          return _eq(actual, v);
        case 'not':
          return !_eq(actual, v);
        case 'gte':
          return actual is num && v is num && actual >= v;
        case 'lte':
          return actual is num && v is num && actual <= v;
        case 'gt':
          return actual is num && v is num && actual > v;
        case 'lt':
          return actual is num && v is num && actual < v;
        case 'exists':
          return actual != null;
        case 'includes':
          return actual is String && v is String && actual.contains(v);
        case 'startsWith':
          return actual is String && v is String && actual.startsWith(v);
        case 'endsWith':
          return actual is String && v is String && actual.endsWith(v);
        default:
          return true; // unknown condition: show rather than hide a param
      }
    }
    return _eq(actual, want);
  }

  static bool _eq(Object? a, Object? b) {
    if (a is num && b is num) return a == b;
    return a == b;
  }
}

/// A connection port of a node.
class PortSpec {
  const PortSpec(this.type, this.label);
  final String type;
  final String label;
  bool get isMain => type == 'main';
}

/// Node inputs. Falls back to one `main` input for unsupported types, none for triggers.
List<PortSpec> inputPorts(FlowNode node, NodeTypeInfo? info) {
  if (info == null) {
    return categoryOf(node.type) == NodeCategory.trigger
        ? const []
        : const [PortSpec('main', '')];
  }
  final expr = info.inputsExpr;
  if (expr != null) {
    if (expr.contains(r'$parameter.numberInputs')) {
      final n = _asInt(node.parameters['numberInputs']) ?? 2;
      return [
        for (var i = 0; i < n.clamp(1, 50); i++) PortSpec('main', '${i + 1}'),
      ];
    }
    return const [PortSpec('main', '')];
  }
  return [for (final t in info.inputs) PortSpec(t, '')];
}

/// Node outputs. [connected] is the highest connected slot, used when the
/// declaration is an expression we cannot evaluate or the type is unknown.
List<PortSpec> outputPorts(FlowNode node, NodeTypeInfo? info, int connected) {
  if (info == null) {
    return [for (var i = 0; i <= (connected < 0 ? 0 : connected); i++) const PortSpec('main', '')];
  }
  final expr = info.outputsExpr;
  if (expr != null) {
    final p = node.parameters;
    if (expr.contains(r'$parameter.rules')) {
      final labels = <String>[];
      if (p['mode'] == 'expression') {
        final n = _asInt(p['numberOutputs']) ?? 4;
        for (var i = 0; i < n.clamp(1, 50); i++) {
          labels.add('$i');
        }
      } else {
        final rules = p['rules'];
        final values = rules is Map && rules['values'] is List
            ? rules['values'] as List
            : const [];
        for (var i = 0; i < values.length; i++) {
          final v = values[i];
          final key = v is Map ? v['outputKey'] : null;
          labels.add(key is String && key.isNotEmpty ? key : '$i');
        }
        final opts = p['options'];
        if (opts is Map && opts['fallbackOutput'] == 'extra') {
          labels.add('Fallback');
        }
      }
      if (labels.isEmpty) labels.add('0');
      return [for (final l in labels) PortSpec('main', l)];
    }
    final n = connected + 1;
    return [for (var i = 0; i < (n < 1 ? 1 : n); i++) const PortSpec('main', '')];
  }
  return [
    for (var i = 0; i < info.outputs.length; i++)
      PortSpec(
        info.outputs[i],
        i < info.outputNames.length ? info.outputNames[i] : '',
      ),
  ];
}

int? _asInt(Object? v) {
  if (v is num) return v.toInt();
  if (v is String) return int.tryParse(v);
  return null;
}
