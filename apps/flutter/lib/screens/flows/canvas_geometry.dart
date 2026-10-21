import 'dart:math' as math;
import 'dart:ui';

import 'model/node_types.dart';
import 'model/workflow_model.dart';

/// Canvas units; all node geometry derives from these.
const double kNodeWidth = 200;
const double kNodeMinHeight = 64;
const double kPortRow = 24;

/// Transparent margin around a card so ports that stick out stay hit-testable.
/// Sized for the touch port hit box (half of 48).
const double kPortPad = 24;

/// Port hit box side in canvas units: pointer vs finger.
const double kPortHitMouse = 24;
const double kPortHitTouch = 48;

enum PortSide { left, right, top, bottom }

/// One port of one node. [index] counts within its connection [type].
class PortRef {
  const PortRef(this.node, this.output, this.type, this.index);
  final String node;
  final bool output;
  final String type;
  final int index;

  @override
  bool operator ==(Object other) =>
      other is PortRef &&
      other.node == node &&
      other.output == output &&
      other.type == type &&
      other.index == index;

  @override
  int get hashCode => Object.hash(node, output, type, index);
}

class PortSlot {
  const PortSlot(this.ref, this.spec, this.anchor, this.side);
  final PortRef ref;
  final PortSpec spec;

  /// Center in canvas coordinates.
  final Offset anchor;
  final PortSide side;
}

/// Placed node: rect + ports in canvas coordinates.
class NodeLayout {
  NodeLayout(this.node, this.info, this.rect, this.inputs, this.outputs);

  factory NodeLayout.compute(FlowNode node, NodeTypeInfo? info, int connected) {
    final ins = inputPorts(node, info);
    final outs = outputPorts(node, info, connected);
    final mainIn = ins.where((p) => p.isMain).length;
    final mainOut = outs.where((p) => p.isMain).length;
    final rows = math.max(mainIn, mainOut);
    final h = math.max(kNodeMinHeight, 20.0 + rows * kPortRow);
    final pos = node.position;
    final rect = Rect.fromLTWH(pos.x, pos.y, kNodeWidth, h);
    return NodeLayout(
      node,
      info,
      rect,
      _slots(node.name, false, ins, rect),
      _slots(node.name, true, outs, rect),
    );
  }

  final FlowNode node;
  final NodeTypeInfo? info;
  final Rect rect;
  final List<PortSlot> inputs;
  final List<PortSlot> outputs;

  /// No registry entry for this exact type + version: card shows the warning.
  bool get unsupported =>
      info == null || !info!.supportsVersion(node.typeVersion);

  PortSlot? input(String type, int index) => _find(inputs, type, index);
  PortSlot? output(String type, int index) => _find(outputs, type, index);

  static PortSlot? _find(List<PortSlot> l, String type, int index) {
    for (final p in l) {
      if (p.ref.type == type && p.ref.index == index) return p;
    }
    return null;
  }

  static List<PortSlot> _slots(
    String node,
    bool output,
    List<PortSpec> specs,
    Rect r,
  ) {
    final main = specs.where((p) => p.isMain).length;
    final other = specs.length - main;
    final counters = <String, int>{};
    var mi = 0;
    var oi = 0;
    final out = <PortSlot>[];
    for (final s in specs) {
      final idx = counters[s.type] ?? 0;
      counters[s.type] = idx + 1;
      final ref = PortRef(node, output, s.type, idx);
      if (s.isMain) {
        final y = r.top + r.height * (mi + 0.5) / main;
        mi++;
        out.add(
          PortSlot(
            ref,
            s,
            Offset(output ? r.right : r.left, y),
            output ? PortSide.right : PortSide.left,
          ),
        );
      } else {
        // Sub-node ports (ai_*): inputs hang below, outputs plug in on top.
        final x = r.left + r.width * (oi + 0.5) / other;
        oi++;
        out.add(
          PortSlot(
            ref,
            s,
            Offset(x, output ? r.top : r.bottom),
            output ? PortSide.top : PortSide.bottom,
          ),
        );
      }
    }
    return out;
  }
}

Offset _dir(PortSide s) => switch (s) {
  PortSide.left => const Offset(-1, 0),
  PortSide.right => const Offset(1, 0),
  PortSide.top => const Offset(0, -1),
  PortSide.bottom => const Offset(0, 1),
};

/// Cubic bezier between two ports, leaving each side along its normal.
Path edgePath(Offset a, PortSide aSide, Offset b, PortSide bSide) {
  final k = math.max(40.0, (a - b).distance * 0.35);
  final c1 = a + _dir(aSide) * k;
  final c2 = b + _dir(bSide) * k;
  return Path()
    ..moveTo(a.dx, a.dy)
    ..cubicTo(c1.dx, c1.dy, c2.dx, c2.dy, b.dx, b.dy);
}

Offset bezierAt(Offset a, Offset c1, Offset c2, Offset b, double t) {
  final u = 1 - t;
  return a * (u * u * u) +
      c1 * (3 * u * u * t) +
      c2 * (3 * u * t * t) +
      b * (t * t * t);
}

/// Sampled points of [edgePath] for hit testing and the midpoint handle.
List<Offset> edgePoints(
  Offset a,
  PortSide aSide,
  Offset b,
  PortSide bSide, {
  int steps = 24,
}) {
  final k = math.max(40.0, (a - b).distance * 0.35);
  final c1 = a + _dir(aSide) * k;
  final c2 = b + _dir(bSide) * k;
  return [for (var i = 0; i <= steps; i++) bezierAt(a, c1, c2, b, i / steps)];
}

double distanceToPolyline(Offset p, List<Offset> pts) {
  var best = double.infinity;
  for (var i = 0; i < pts.length - 1; i++) {
    final a = pts[i];
    final b = pts[i + 1];
    final ab = b - a;
    final len2 = ab.dx * ab.dx + ab.dy * ab.dy;
    final t = len2 == 0
        ? 0.0
        : (((p - a).dx * ab.dx + (p - a).dy * ab.dy) / len2).clamp(0.0, 1.0);
    final d = (p - (a + ab * t)).distance;
    if (d < best) best = d;
  }
  return best;
}

/// Edge endpoints for [e] given placed nodes; null when a node is missing.
/// A port the description does not declare (stale connection, expression
/// outputs we cannot evaluate) falls back to the node's mid-side so the edge
/// stays visible and deletable.
({Offset a, PortSide aSide, Offset b, PortSide bSide})? edgeEnds(
  FlowEdge e,
  Map<String, NodeLayout> layouts,
) {
  final sl = layouts[e.src];
  final dl = layouts[e.dst];
  if (sl == null || dl == null) return null;
  final s = sl.output(e.type, e.srcIndex);
  final d = dl.input(e.type, e.dstIndex);
  return (
    a: s?.anchor ?? Offset(sl.rect.right, sl.rect.center.dy),
    aSide: s?.side ?? PortSide.right,
    b: d?.anchor ?? Offset(dl.rect.left, dl.rect.center.dy),
    bSide: d?.side ?? PortSide.left,
  );
}
