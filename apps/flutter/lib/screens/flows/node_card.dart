import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show HardwareKeyboard;

import '../../theme/tokens.dart';
import 'canvas_geometry.dart';
import 'model/node_types.dart';

Color categoryColor(NodeCategory c) => switch (c) {
  NodeCategory.trigger => HkTokens.nodeTrigger,
  NodeCategory.ai => HkTokens.nodeAi,
  NodeCategory.logic => HkTokens.nodeLogic,
  NodeCategory.data => HkTokens.nodeData,
  NodeCategory.integration => HkTokens.nodeIntegration,
};

IconData categoryIcon(NodeCategory c) => switch (c) {
  NodeCategory.trigger => Icons.bolt,
  NodeCategory.ai => Icons.auto_awesome,
  NodeCategory.logic => Icons.call_split,
  NodeCategory.data => Icons.data_object,
  NodeCategory.integration => Icons.extension_outlined,
};

/// Gesture hooks for one node; the canvas owns the state.
class NodeCardCallbacks {
  const NodeCardCallbacks({
    required this.onTap,
    required this.onDoubleTap,
    required this.onDragStart,
    required this.onDragUpdate,
    required this.onDragEnd,
    required this.onPortDragStart,
    required this.onPortDragUpdate,
    required this.onPortDragEnd,
  });

  final void Function(bool additive) onTap;
  final VoidCallback onDoubleTap;
  final void Function(Offset globalPos) onDragStart;
  final void Function(Offset globalPos) onDragUpdate;
  final VoidCallback onDragEnd;
  final void Function(PortRef port, Offset globalPos) onPortDragStart;
  final void Function(Offset globalPos) onPortDragUpdate;
  final void Function(Offset globalPos) onPortDragEnd;
}

/// Widget for one node, sized `rect.inflate(kPortPad)`: the card plus a margin
/// holding the ports.
class NodeCard extends StatelessWidget {
  const NodeCard({
    super.key,
    required this.layout,
    required this.selected,
    required this.callbacks,
    this.pendingFrom,
  });

  final NodeLayout layout;
  final bool selected;
  final NodeCardCallbacks callbacks;

  /// Port being dragged from; opposite-kind ports light up as drop targets.
  final PortRef? pendingFrom;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final node = layout.node;
    final cat = categoryOf(node.type, group: layout.info?.group ?? const []);
    final stripe = categoryColor(cat);
    final unsupported = layout.unsupported;
    final r = layout.rect;
    final labeledOuts = layout.outputs.where((p) => p.spec.isMain && p.spec.label.isNotEmpty);
    final shortName = layout.info?.displayName ?? shortType(node.type);

    final border = selected
        ? hk.accent
        : unsupported
        ? hk.warn
        : hk.border;

    final card = GestureDetector(
      behavior: HitTestBehavior.opaque,
      onTap: () => callbacks.onTap(
        HardwareKeyboard.instance.isShiftPressed ||
            HardwareKeyboard.instance.isControlPressed ||
            HardwareKeyboard.instance.isMetaPressed,
      ),
      onDoubleTap: callbacks.onDoubleTap,
      onPanStart: (d) => callbacks.onDragStart(d.globalPosition),
      onPanUpdate: (d) => callbacks.onDragUpdate(d.globalPosition),
      onPanEnd: (_) => callbacks.onDragEnd(),
      onPanCancel: callbacks.onDragEnd,
      child: Opacity(
        opacity: node.disabled ? 0.5 : 1,
        child: Container(
          width: r.width,
          height: r.height,
          decoration: BoxDecoration(
            color: hk.surface,
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            border: Border.all(color: border, width: selected ? 2 : 1),
          ),
          clipBehavior: Clip.antiAlias,
          child: Row(
            children: [
              Container(width: 4, color: stripe),
              const SizedBox(width: HkTokens.s2),
              Icon(categoryIcon(cat), size: 20, color: stripe),
              const SizedBox(width: HkTokens.s2),
              Expanded(
                child: Padding(
                  padding: EdgeInsets.only(right: labeledOuts.isEmpty ? HkTokens.s2 : 44),
                  child: Column(
                    mainAxisAlignment: MainAxisAlignment.center,
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        node.name,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.bodyMedium?.copyWith(
                          fontWeight: FontWeight.w600,
                          decoration: node.disabled ? TextDecoration.lineThrough : null,
                        ),
                      ),
                      Text(
                        unsupported ? 'Unsupported: ${shortType(node.type)}' : shortName,
                        maxLines: 1,
                        overflow: TextOverflow.ellipsis,
                        style: text.bodySmall?.copyWith(
                          color: unsupported ? hk.warn : hk.textMuted,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
              if (unsupported)
                Padding(
                  padding: const EdgeInsets.only(right: HkTokens.s2),
                  child: Tooltip(
                    message: layout.info == null
                        ? 'Node type not available in this build. It is kept as-is and fails at run time.'
                        : 'typeVersion ${node.typeVersion} is not supported. It is kept as-is.',
                    child: Icon(Icons.warning_amber_rounded, size: 18, color: hk.warn),
                  ),
                ),
            ],
          ),
        ),
      ),
    );

    return SizedBox(
      width: r.width + 2 * kPortPad,
      height: r.height + 2 * kPortPad,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          Positioned(left: kPortPad, top: kPortPad, child: card),
          for (final p in layout.outputs.where((p) => p.spec.isMain && p.spec.label.isNotEmpty))
            Positioned(
              left: kPortPad + r.width - 8 - 40,
              top: p.anchor.dy - r.top + kPortPad - 8,
              width: 40,
              child: IgnorePointer(
                child: Text(
                  p.spec.label,
                  textAlign: TextAlign.right,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: text.bodySmall?.copyWith(color: hk.textMuted, fontSize: 12),
                ),
              ),
            ),
          for (final p in [...layout.inputs, ...layout.outputs])
            Positioned(
              left: p.anchor.dx - r.left + kPortPad - 12,
              top: p.anchor.dy - r.top + kPortPad - 12,
              width: 24,
              height: 24,
              child: _PortDot(
                slot: p,
                target: _isTarget(p),
                onStart: (g) => callbacks.onPortDragStart(p.ref, g),
                onUpdate: callbacks.onPortDragUpdate,
                onEnd: callbacks.onPortDragEnd,
              ),
            ),
        ],
      ),
    );
  }

  bool _isTarget(PortSlot p) {
    final from = pendingFrom;
    if (from == null) return false;
    return from.node != p.ref.node &&
        from.output != p.ref.output &&
        from.type == p.ref.type;
  }
}

class _PortDot extends StatefulWidget {
  const _PortDot({
    required this.slot,
    required this.target,
    required this.onStart,
    required this.onUpdate,
    required this.onEnd,
  });

  final PortSlot slot;
  final bool target;
  final void Function(Offset) onStart;
  final void Function(Offset) onUpdate;
  final void Function(Offset) onEnd;

  @override
  State<_PortDot> createState() => _PortDotState();
}

class _PortDotState extends State<_PortDot> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final active = _hover || widget.target;
    final size = active ? 14.0 : 10.0;
    final fill = active ? hk.accent : hk.textMuted;
    final dot = Container(
      width: size,
      height: size,
      decoration: BoxDecoration(
        color: widget.slot.spec.isMain ? fill : hk.surface,
        border: Border.all(color: fill, width: 2),
        shape: widget.slot.spec.isMain ? BoxShape.circle : BoxShape.rectangle,
        borderRadius: widget.slot.spec.isMain ? null : BorderRadius.circular(2),
      ),
    );
    return MouseRegion(
      cursor: SystemMouseCursors.precise,
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: GestureDetector(
        behavior: HitTestBehavior.opaque,
        onPanStart: (d) => widget.onStart(d.globalPosition),
        onPanUpdate: (d) => widget.onUpdate(d.globalPosition),
        onPanEnd: (d) => widget.onEnd(d.globalPosition),
        child: Center(
          child: widget.slot.spec.isMain
              ? dot
              : Transform.rotate(angle: 0.785398, child: dot),
        ),
      ),
    );
  }
}
