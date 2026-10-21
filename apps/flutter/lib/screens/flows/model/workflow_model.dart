import 'dart:convert';

/// Editable view of an n8n workflow JSON (DESIGN §3.9: n8n JSON is the native format).
///
/// Lossless by construction: every node keeps its full raw map, the workflow
/// keeps every top-level key (settings, pinData, meta, ...) in original order,
/// and connection targets keep their raw maps. Edits copy-on-write, so old
/// instances stay valid snapshots for undo/redo.
///
/// Pure Dart: no Flutter imports, so the model is unit-testable without a binding.

typedef Pos = ({double x, double y});

class FlowFormatException implements Exception {
  FlowFormatException(this.message);
  final String message;
  @override
  String toString() => message;
}

/// Deep copy of a JSON value (maps/lists/scalars), preserving key order.
Object? deepCopy(Object? v) {
  if (v is Map) {
    return <String, dynamic>{
      for (final e in v.entries) e.key as String: deepCopy(e.value),
    };
  }
  if (v is List) return [for (final e in v) deepCopy(e)];
  return v;
}

/// One node. [raw] is the exact JSON object; never mutate it, use the `with*` helpers.
class FlowNode {
  FlowNode(Map<String, dynamic> raw) : raw = Map.unmodifiable(raw);

  factory FlowNode.create({
    required String id,
    required String name,
    required String type,
    required num typeVersion,
    required Pos position,
    Map<String, dynamic>? parameters,
  }) => FlowNode({
    'parameters': parameters ?? <String, dynamic>{},
    'id': id,
    'name': name,
    'type': type,
    'typeVersion': typeVersion,
    'position': [position.x.round(), position.y.round()],
  });

  final Map<String, dynamic> raw;

  String? get id => raw['id'] as String?;
  String get name => raw['name'] as String;
  String get type => (raw['type'] as String?) ?? '';
  num get typeVersion => (raw['typeVersion'] as num?) ?? 1;
  bool get disabled => raw['disabled'] == true;
  String? get notes => raw['notes'] as String?;

  Pos get position {
    final p = raw['position'];
    if (p is List && p.length >= 2 && p[0] is num && p[1] is num) {
      return (x: (p[0] as num).toDouble(), y: (p[1] as num).toDouble());
    }
    return (x: 0, y: 0);
  }

  Map<String, dynamic> get parameters {
    final p = raw['parameters'];
    return p is Map<String, dynamic> ? p : const <String, dynamic>{};
  }

  FlowNode set(String key, Object? value) =>
      FlowNode(Map<String, dynamic>.of(raw)..[key] = value);

  FlowNode remove(String key) =>
      FlowNode(Map<String, dynamic>.of(raw)..remove(key));

  FlowNode withName(String name) => set('name', name);

  /// n8n stores integer coordinates.
  FlowNode withPosition(Pos p) => set('position', [p.x.round(), p.y.round()]);

  FlowNode withParameters(Map<String, dynamic> params) =>
      set('parameters', deepCopy(params));

  Map<String, dynamic> toJson() => deepCopy(raw)! as Map<String, dynamic>;
}

/// One connection endpoint; [raw] keeps unknown keys.
class ConnTarget {
  ConnTarget(Map<String, dynamic> raw) : raw = Map.unmodifiable(raw);

  factory ConnTarget.create(String node, String type, int index) =>
      ConnTarget({'node': node, 'type': type, 'index': index});

  final Map<String, dynamic> raw;

  String get node => raw['node'] as String;
  String get type => (raw['type'] as String?) ?? 'main';
  int get index => (raw['index'] as num?)?.toInt() ?? 0;

  ConnTarget withNode(String node) =>
      ConnTarget(Map<String, dynamic>.of(raw)..['node'] = node);

  Map<String, dynamic> toJson() => deepCopy(raw)! as Map<String, dynamic>;
}

/// A resolved edge: output [srcIndex] of [src] feeds input [dstIndex] of [dst].
class FlowEdge {
  const FlowEdge({
    required this.src,
    required this.srcIndex,
    required this.dst,
    required this.dstIndex,
    this.type = 'main',
  });

  final String src;
  final int srcIndex;
  final String dst;
  final int dstIndex;

  /// Connection type, shared by both ends (`main`, `ai_tool`, ...).
  final String type;

  @override
  bool operator ==(Object other) =>
      other is FlowEdge &&
      other.src == src &&
      other.srcIndex == srcIndex &&
      other.dst == dst &&
      other.dstIndex == dstIndex &&
      other.type == type;

  @override
  int get hashCode => Object.hash(src, srcIndex, dst, dstIndex, type);

  @override
  String toString() => '$src[$type:$srcIndex] -> $dst[$dstIndex]';
}

/// `{ nodeName: { type: [ [target, ...] | null, ... ] } }`, outputs indexed by slot.
class FlowConnections {
  FlowConnections._(this._d);

  factory FlowConnections.empty() => FlowConnections._({});

  factory FlowConnections.fromJson(Object? json) {
    if (json == null) return FlowConnections.empty();
    if (json is! Map) {
      throw FlowFormatException('`connections` must be an object');
    }
    final out = <String, Map<String, List<List<ConnTarget>?>>>{};
    for (final n in json.entries) {
      final byType = n.value;
      if (byType is! Map) {
        throw FlowFormatException('connections of `${n.key}` must be an object');
      }
      final types = <String, List<List<ConnTarget>?>>{};
      for (final t in byType.entries) {
        final slots = t.value;
        if (slots is! List) {
          throw FlowFormatException(
            'connections `${n.key}.${t.key}` must be a list',
          );
        }
        types[t.key as String] = [
          for (final slot in slots)
            if (slot == null)
              null
            else if (slot is List)
              [
                for (final tg in slot)
                  if (tg is Map && tg['node'] is String)
                    ConnTarget(Map<String, dynamic>.from(tg))
                  else
                    throw FlowFormatException(
                      'bad connection target in `${n.key}`',
                    ),
              ]
            else
              throw FlowFormatException(
                'connection slot of `${n.key}` must be a list',
              ),
        ];
      }
      out[n.key as String] = types;
    }
    return FlowConnections._(out);
  }

  final Map<String, Map<String, List<List<ConnTarget>?>>> _d;

  bool get isEmpty => _d.isEmpty;

  /// Structure copy; [ConnTarget]s are immutable so they are shared.
  FlowConnections clone() => FlowConnections._({
    for (final n in _d.entries)
      n.key: {
        for (final t in n.value.entries)
          t.key: [for (final s in t.value) s == null ? null : List.of(s)],
      },
  });

  Map<String, dynamic> toJson() => {
    for (final n in _d.entries)
      n.key: {
        for (final t in n.value.entries)
          t.key: [
            for (final s in t.value)
              s == null ? null : [for (final tg in s) tg.toJson()],
          ],
      },
  };

  Iterable<FlowEdge> get edges sync* {
    for (final n in _d.entries) {
      for (final t in n.value.entries) {
        for (var i = 0; i < t.value.length; i++) {
          for (final tg in t.value[i] ?? const <ConnTarget>[]) {
            yield FlowEdge(
              src: n.key,
              srcIndex: i,
              dst: tg.node,
              dstIndex: tg.index,
              type: t.key,
            );
          }
        }
      }
    }
  }

  List<ConnTarget> targets(String src, String type, int output) {
    final slots = _d[src]?[type];
    if (slots == null || output >= slots.length) return const [];
    return slots[output] ?? const [];
  }

  bool contains(FlowEdge e) => targets(e.src, e.type, e.srcIndex).any(
    (t) => t.node == e.dst && t.type == e.type && t.index == e.dstIndex,
  );

  /// Highest connected output slot of [node] for [type], or -1.
  int maxOutput(String node, String type) {
    final slots = _d[node]?[type];
    if (slots == null) return -1;
    for (var i = slots.length - 1; i >= 0; i--) {
      if (slots[i]?.isNotEmpty ?? false) return i;
    }
    return -1;
  }

  // Mutators: call on a [clone] only.

  bool add(FlowEdge e) {
    if (contains(e)) return false;
    final slots = (_d[e.src] ??= {})[e.type] ??= [];
    while (slots.length <= e.srcIndex) {
      slots.add(null);
    }
    slots[e.srcIndex] = [
      ...?slots[e.srcIndex],
      ConnTarget.create(e.dst, e.type, e.dstIndex),
    ];
    return true;
  }

  bool remove(FlowEdge e) {
    final slots = _d[e.src]?[e.type];
    if (slots == null || e.srcIndex >= slots.length) return false;
    final before = slots[e.srcIndex]?.length ?? 0;
    final next = [
      for (final t in slots[e.srcIndex] ?? const <ConnTarget>[])
        if (!(t.node == e.dst && t.type == e.type && t.index == e.dstIndex)) t,
    ];
    if (next.length == before) return false;
    slots[e.srcIndex] = next;
    _prune(e.src, e.type);
    return true;
  }

  /// Drops every edge touching [name], in or out.
  void removeNode(String name) {
    _d.remove(name);
    for (final src in _d.keys.toList()) {
      for (final type in _d[src]!.keys.toList()) {
        final slots = _d[src]![type]!;
        for (var i = 0; i < slots.length; i++) {
          final s = slots[i];
          if (s == null) continue;
          slots[i] = [
            for (final t in s)
              if (t.node != name) t,
          ];
        }
        _prune(src, type);
      }
    }
  }

  void renameNode(String from, String to) {
    final rebuilt = <String, Map<String, List<List<ConnTarget>?>>>{};
    for (final n in _d.entries) {
      for (final t in n.value.values) {
        for (var i = 0; i < t.length; i++) {
          final s = t[i];
          if (s == null) continue;
          t[i] = [for (final tg in s) tg.node == from ? tg.withNode(to) : tg];
        }
      }
      rebuilt[n.key == from ? to : n.key] = n.value;
    }
    _d
      ..clear()
      ..addAll(rebuilt);
  }

  /// Trailing empty slots and emptied containers are noise in n8n exports.
  void _prune(String src, String type) {
    final slots = _d[src]?[type];
    if (slots == null) return;
    while (slots.isNotEmpty && (slots.last?.isEmpty ?? true)) {
      slots.removeLast();
    }
    if (slots.isEmpty) _d[src]!.remove(type);
    if (_d[src]!.isEmpty) _d.remove(src);
  }
}

/// A whole workflow document.
class FlowWorkflow {
  FlowWorkflow._(this._keys, this._top, this.nodes, this.connections);

  factory FlowWorkflow.empty({String name = 'My workflow'}) => FlowWorkflow._(
    ['name', 'nodes', 'connections', 'active', 'settings'],
    {
      'name': name,
      'active': false,
      'settings': <String, dynamic>{'executionOrder': 'v1'},
    },
    const [],
    FlowConnections.empty(),
  );

  factory FlowWorkflow.fromJson(Object? json) {
    if (json is! Map) {
      throw FlowFormatException('workflow JSON must be an object');
    }
    final rawNodes = json['nodes'];
    if (rawNodes is! List) {
      throw FlowFormatException('workflow JSON has no `nodes` array');
    }
    final nodes = [for (final n in rawNodes) _parseNode(n)];
    final keys = [for (final k in json.keys) k as String];
    final top = <String, dynamic>{
      for (final e in json.entries)
        if (e.key != 'nodes' && e.key != 'connections')
          e.key as String: deepCopy(e.value),
    };
    if (!keys.contains('nodes')) keys.add('nodes');
    if (!keys.contains('connections')) keys.add('connections');
    return FlowWorkflow._(
      keys,
      top,
      List.unmodifiable(nodes),
      FlowConnections.fromJson(json['connections']),
    );
  }

  factory FlowWorkflow.parse(String text) {
    final Object? json;
    try {
      json = jsonDecode(text);
    } on FormatException catch (e) {
      throw FlowFormatException('invalid JSON: ${e.message}');
    }
    return FlowWorkflow.fromJson(json);
  }

  static FlowNode _parseNode(Object? n) {
    if (n is! Map || n['name'] is! String) {
      throw FlowFormatException('every node needs a string `name`');
    }
    return FlowNode(deepCopy(n)! as Map<String, dynamic>);
  }

  final List<String> _keys;
  final Map<String, dynamic> _top;
  final List<FlowNode> nodes;
  final FlowConnections connections;

  String get name => (_top['name'] as String?) ?? '';
  bool get active => _top['active'] == true;

  /// Top-level value outside nodes/connections (settings, pinData, meta, ...).
  Object? field(String key) => _top[key];

  Map<String, dynamic> get pinData {
    final p = _top['pinData'];
    return p is Map<String, dynamic> ? p : const <String, dynamic>{};
  }

  FlowNode? node(String name) {
    for (final n in nodes) {
      if (n.name == name) return n;
    }
    return null;
  }

  bool hasNode(String name) => node(name) != null;

  FlowWorkflow copyWith({
    List<FlowNode>? nodes,
    FlowConnections? connections,
    Map<String, dynamic>? top,
  }) => FlowWorkflow._(
    _keys,
    top ?? _top,
    nodes == null ? this.nodes : List.unmodifiable(nodes),
    connections ?? this.connections,
  );

  FlowWorkflow withField(String key, Object? value) {
    final top = Map<String, dynamic>.of(_top)..[key] = value;
    final keys = _keys.contains(key) ? _keys : [..._keys, key];
    return FlowWorkflow._(keys, top, nodes, connections);
  }

  FlowWorkflow withName(String name) => withField('name', name);

  FlowWorkflow withoutField(String key) {
    final top = Map<String, dynamic>.of(_top)..remove(key);
    return FlowWorkflow._(
      [
        for (final k in _keys)
          if (k != key) k,
      ],
      top,
      nodes,
      connections,
    );
  }

  Map<String, dynamic> toJson() => {
    for (final k in _keys)
      if (k == 'nodes')
        k: [for (final n in nodes) n.toJson()]
      else if (k == 'connections')
        k: connections.toJson()
      else if (_top.containsKey(k))
        k: deepCopy(_top[k]),
  };

  String toJsonString({bool pretty = true}) => pretty
      ? const JsonEncoder.withIndent('  ').convert(toJson())
      : jsonEncode(toJson());

  /// Structural problems (same checks as hk-flow `Workflow::validate`).
  List<String> validate() {
    final issues = <String>[];
    final names = <String>{};
    for (final n in nodes) {
      if (!names.add(n.name)) issues.add('duplicate node name `${n.name}`');
    }
    for (final e in connections.edges) {
      if (!names.contains(e.src)) {
        issues.add('connection from unknown node `${e.src}`');
      }
      if (!names.contains(e.dst)) {
        issues.add('connection `${e.src}` -> unknown node `${e.dst}`');
      }
    }
    for (final k in pinData.keys) {
      if (!names.contains(k)) issues.add('pinData for unknown node `$k`');
    }
    return issues;
  }
}
