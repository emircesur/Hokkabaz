import 'dart:convert';
import 'dart:math';

import 'workflow_model.dart';

/// n8n clipboard format: `{ "nodes": [...], "connections": {...} }` (what n8n
/// puts on the clipboard when copying a node selection). Parsing is lenient:
/// a full workflow export, a bare node array and a single node object all work.
class ClipboardPayload {
  ClipboardPayload(this.nodes, this.connections, this.pinData);

  final List<FlowNode> nodes;
  final FlowConnections connections;
  final Map<String, dynamic> pinData;
}

/// Returns null when [text] is not n8n JSON at all (plain text paste); throws
/// [FlowFormatException] when it is JSON that looks like n8n but is broken.
ClipboardPayload? parseClipboard(String text) {
  final t = text.trim();
  if (t.isEmpty || (!t.startsWith('{') && !t.startsWith('['))) return null;
  final Object? j;
  try {
    j = jsonDecode(t);
  } on FormatException {
    return null;
  }
  if (j is List) return _fromNodes(j, null, null);
  if (j is! Map) return null;
  if (j['nodes'] is List) {
    return _fromNodes(j['nodes'] as List, j['connections'], j['pinData']);
  }
  if (j['type'] is String && j['name'] is String) {
    return _fromNodes([j], null, null);
  }
  return null;
}

ClipboardPayload _fromNodes(List raw, Object? conns, Object? pin) {
  final nodes = <FlowNode>[];
  for (final n in raw) {
    if (n is! Map || n['name'] is! String || n['type'] is! String) {
      throw FlowFormatException(
        'clipboard node needs string `name` and `type`',
      );
    }
    nodes.add(FlowNode(deepCopy(n)! as Map<String, dynamic>));
  }
  if (nodes.isEmpty) throw FlowFormatException('clipboard has no nodes');
  return ClipboardPayload(
    nodes,
    FlowConnections.fromJson(conns),
    pin is Map ? deepCopy(pin)! as Map<String, dynamic> : <String, dynamic>{},
  );
}

/// JSON for copying [names] out of [wf]: only edges with both ends selected.
String exportNodes(FlowWorkflow wf, Iterable<String> names) {
  final set = names.toSet();
  final nodes = [
    for (final n in wf.nodes)
      if (set.contains(n.name)) n,
  ];
  final conns = wf.connections.clone();
  for (final n in wf.nodes) {
    if (!set.contains(n.name)) conns.removeNode(n.name);
  }
  final out = <String, dynamic>{
    'nodes': [for (final n in nodes) n.toJson()],
    'connections': conns.toJson(),
  };
  final pins = {
    for (final e in wf.pinData.entries)
      if (set.contains(e.key)) e.key: deepCopy(e.value),
  };
  if (pins.isNotEmpty) out['pinData'] = pins;
  return const JsonEncoder.withIndent('  ').convert(out);
}

final _rng = () {
  try {
    return Random.secure();
  } catch (_) {
    return Random();
  }
}();

/// RFC 4122 v4 id, the format n8n uses for node ids.
String newNodeId() {
  final b = List<int>.generate(16, (_) => _rng.nextInt(256));
  b[6] = (b[6] & 0x0f) | 0x40;
  b[8] = (b[8] & 0x3f) | 0x80;
  String h(int from, int to) => b
      .sublist(from, to)
      .map((e) => e.toRadixString(16).padLeft(2, '0'))
      .join();
  return '${h(0, 4)}-${h(4, 6)}-${h(6, 8)}-${h(8, 10)}-${h(10, 16)}';
}

/// First free name for [base]: `Set`, `Set1`, `Set2`, ... (n8n style).
String uniqueName(String base, Iterable<String> taken) {
  final used = taken.toSet();
  if (!used.contains(base)) return base;
  final stem = base.replaceFirst(RegExp(r'\d+$'), '');
  for (var i = 1; ; i++) {
    final c = '$stem$i';
    if (!used.contains(c)) return c;
  }
}

class PasteResult {
  PasteResult(this.workflow, this.names);
  final FlowWorkflow workflow;

  /// Names of the inserted nodes, after collision renames.
  final List<String> names;
}

/// Insert [p] into [wf]: fresh ids/webhookIds, collision-free names, edges and
/// pinData remapped, block moved so its top-left lands on [at] (or nudged
/// +40,+40 when [at] is null so a paste in place does not hide the original).
PasteResult pasteInto(FlowWorkflow wf, ClipboardPayload p, {Pos? at}) {
  final taken = wf.nodes.map((n) => n.name).toSet();
  final rename = <String, String>{};
  for (final n in p.nodes) {
    final name = uniqueName(n.name, taken);
    taken.add(name);
    rename[n.name] = name;
  }
  var minX = double.infinity;
  var minY = double.infinity;
  for (final n in p.nodes) {
    final pos = n.position;
    if (pos.x < minX) minX = pos.x;
    if (pos.y < minY) minY = pos.y;
  }
  final dx = at == null ? 40.0 : at.x - minX;
  final dy = at == null ? 40.0 : at.y - minY;

  final added = <FlowNode>[];
  for (final n in p.nodes) {
    var m = n.withName(rename[n.name]!).set('id', newNodeId()).withPosition((
      x: n.position.x + dx,
      y: n.position.y + dy,
    ));
    if (m.raw.containsKey('webhookId')) m = m.set('webhookId', newNodeId());
    added.add(m);
  }

  final conns = wf.connections.clone();
  for (final e in p.connections.edges) {
    final s = rename[e.src];
    final d = rename[e.dst];
    if (s == null || d == null) continue;
    conns.add(
      FlowEdge(
        src: s,
        srcIndex: e.srcIndex,
        dst: d,
        dstIndex: e.dstIndex,
        type: e.type,
      ),
    );
  }

  var out = wf.copyWith(nodes: [...wf.nodes, ...added], connections: conns);
  if (p.pinData.isNotEmpty) {
    final pins = Map<String, dynamic>.of(wf.pinData);
    for (final e in p.pinData.entries) {
      final to = rename[e.key];
      if (to != null) pins[to] = deepCopy(e.value);
    }
    out = out.withField('pinData', pins);
  }
  return PasteResult(out, [for (final n in added) n.name]);
}
