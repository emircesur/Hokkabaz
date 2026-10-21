import 'dart:math' as math;

import 'package:flutter/gestures.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../app/breakpoints.dart';
import '../../theme/tokens.dart';
import 'canvas_geometry.dart';
import 'model/flow_editor.dart';
import 'model/node_types.dart';
import 'model/workflow_model.dart';
import 'node_card.dart';

/// Half the side of the virtual node plane. Nodes live at `canvas + _origin` inside it, so
/// negative n8n coordinates still hit-test (a Stack only hits inside its own box).
const double _origin = 20000;
const double _minScale = 0.2;
const double _maxScale = 2.0;

/// Pan/zoom node canvas over a [FlowEditor] (DESIGN §4.4).
///
/// Gestures: drag background = pan, shift+drag = marquee select, pinch / ctrl+wheel = zoom,
/// wheel = pan, drag node = move (all selected), drag port = connect (drop on empty space
/// opens the node picker), tap edge = select, long-press background = add node here.
class FlowCanvas extends StatefulWidget {
  const FlowCanvas({
    super.key,
    required this.editor,
    required this.types,
    required this.onAddNode,
    required this.onOpenNode,
    required this.onMessage,
    required this.onSave,
  });

  final FlowEditor editor;
  final Map<String, NodeTypeInfo> types;

  /// Open the picker; [from] set when a connection drag ended on empty space.
  final void Function(Pos at, PortRef? from) onAddNode;
  final void Function(String node) onOpenNode;
  final void Function(String message) onMessage;
  final VoidCallback onSave;

  @override
  State<FlowCanvas> createState() => FlowCanvasState();
}

class FlowCanvasState extends State<FlowCanvas> {
  final _focus = FocusNode(debugLabel: 'flow-canvas');
  Offset _pan = const Offset(60, 60);
  double _scale = 1;
  Size _viewport = Size.zero;

  Offset _focal0 = Offset.zero;
  Offset _pan0 = Offset.zero;
  double _scale0 = 1;
  double _pzScale = 1;
  Offset? _marqueeA;
  Offset? _marqueeB;

  Offset _dragStartGlobal = Offset.zero;
  String? _lastTapNode;
  DateTime _lastTapAt = DateTime.fromMillisecondsSinceEpoch(0);

  /// Touch multi-select: after a long-press on a node, plain taps toggle.
  bool _multiMode = false;

  PortRef? _pendingFrom;
  Offset? _pendingPos;
  Offset _pendingStartGlobal = Offset.zero;

  FlowEditor get _ed => widget.editor;

  @override
  void dispose() {
    _focus.dispose();
    super.dispose();
  }

  // view helpers

  double get scale => _scale;

  Offset _toCanvas(Offset local) => (local - _pan) / _scale;

  Offset _globalToCanvas(Offset global) {
    final box = context.findRenderObject() as RenderBox?;
    return _toCanvas(box?.globalToLocal(global) ?? global);
  }

  /// Canvas point at the middle of the viewport.
  Pos get viewCenter {
    final c = _toCanvas(Offset(_viewport.width / 2, _viewport.height / 2));
    return (x: c.dx, y: c.dy);
  }

  void _zoomAt(Offset screenPt, double factor) {
    final next = (_scale * factor).clamp(_minScale, _maxScale);
    final anchor = _toCanvas(screenPt);
    setState(() {
      _scale = next;
      _pan = screenPt - anchor * next;
    });
  }

  void setZoom(double s) =>
      _zoomAt(Offset(_viewport.width / 2, _viewport.height / 2), s / _scale);

  /// Frame all nodes (or reset when empty).
  void fit() {
    final wf = _ed.workflow;
    if (wf.nodes.isEmpty || _viewport.isEmpty) {
      setState(() {
        _scale = 1;
        _pan = const Offset(60, 60);
      });
      return;
    }
    final layouts = _layouts(wf);
    Rect? b;
    for (final l in layouts.values) {
      b = b == null ? l.rect : b.expandToInclude(l.rect);
    }
    b = b!.inflate(48);
    final s = math
        .min(_viewport.width / b.width, _viewport.height / b.height)
        .clamp(_minScale, 1.2);
    setState(() {
      _scale = s;
      _pan = Offset(
        (_viewport.width - b!.width * s) / 2 - b.left * s,
        (_viewport.height - b.height * s) / 2 - b.top * s,
      );
    });
  }

  Map<String, NodeLayout> _layouts(FlowWorkflow wf) => {
    for (final n in wf.nodes)
      n.name: NodeLayout.compute(
        n,
        widget.types[n.type],
        wf.connections.maxOutput(n.name, 'main'),
      ),
  };

  // background gestures

  void _onScaleStart(ScaleStartDetails d) {
    _focus.requestFocus();
    _focal0 = d.localFocalPoint;
    _pan0 = _pan;
    _scale0 = _scale;
    if (d.pointerCount == 1 && HardwareKeyboard.instance.isShiftPressed) {
      _marqueeA = _toCanvas(d.localFocalPoint);
      _marqueeB = _marqueeA;
    }
  }

  void _onScaleUpdate(ScaleUpdateDetails d) {
    if (_marqueeA != null) {
      setState(() => _marqueeB = _toCanvas(d.localFocalPoint));
      return;
    }
    final s = (_scale0 * d.scale).clamp(_minScale, _maxScale);
    final anchor = (_focal0 - _pan0) / _scale0;
    setState(() {
      _scale = s;
      _pan = d.localFocalPoint - anchor * s;
    });
  }

  void _onScaleEnd(ScaleEndDetails d) {
    final a = _marqueeA;
    final b = _marqueeB;
    if (a == null || b == null) return;
    final box = Rect.fromPoints(a, b);
    final hit = _layouts(_ed.workflow).values
        .where((l) => l.rect.overlaps(box))
        .map((l) => l.node.name);
    _ed.select(hit, additive: HardwareKeyboard.instance.isControlPressed);
    setState(() {
      _marqueeA = null;
      _marqueeB = null;
    });
  }

  void _onTapUp(TapUpDetails d) {
    _focus.requestFocus();
    _multiMode = false;
    if (_pendingFrom != null) {
      setState(() {
        _pendingFrom = null;
        _pendingPos = null;
      });
    }
    final p = _toCanvas(d.localPosition);
    final layouts = _layouts(_ed.workflow);
    FlowEdge? best;
    var bestD = 10 / _scale;
    for (final e in _ed.workflow.connections.edges) {
      final ends = edgeEnds(e, layouts);
      if (ends == null) continue;
      final dist = distanceToPolyline(
        p,
        edgePoints(ends.a, ends.aSide, ends.b, ends.bSide),
      );
      if (dist < bestD) {
        bestD = dist;
        best = e;
      }
    }
    if (best != null) {
      _ed.selectEdge(best);
    } else {
      _ed.clearSelection();
    }
  }

  void _onPointerSignal(PointerSignalEvent e) {
    if (e is! PointerScrollEvent) return;
    GestureBinding.instance.pointerSignalResolver.register(e, (_) {
      final kb = HardwareKeyboard.instance;
      if (kb.isControlPressed || kb.isMetaPressed) {
        _zoomAt(e.localPosition, math.exp(-e.scrollDelta.dy / 300));
      } else {
        setState(
          () => _pan -= kb.isShiftPressed
              ? Offset(e.scrollDelta.dy, 0)
              : e.scrollDelta,
        );
      }
    });
  }

  // node + port gestures

  NodeCardCallbacks _callbacks(String name) => NodeCardCallbacks(
    onTap: (additive) {
      _focus.requestFocus();
      final now = DateTime.now();
      if (_lastTapNode == name &&
          now.difference(_lastTapAt) < const Duration(milliseconds: 350)) {
        _lastTapNode = null;
        widget.onOpenNode(name);
        return;
      }
      _lastTapNode = name;
      _lastTapAt = now;
      if (additive || _multiMode) {
        _ed.toggleSelect(name);
        if (_ed.selection.isEmpty) _multiMode = false;
      } else {
        _ed.select([name]);
      }
    },
    onLongPress: () {
      _focus.requestFocus();
      HapticFeedback.selectionClick();
      _multiMode = true;
      _ed.toggleSelect(name);
    },
    onPortTap: _onPortTap,
    onDragStart: (g) {
      _focus.requestFocus();
      if (!_ed.selection.contains(name)) _ed.select([name]);
      _dragStartGlobal = g;
      _ed.beginDrag(_ed.selection);
    },
    onDragUpdate: (g) {
      final d = (g - _dragStartGlobal) / _scale;
      _ed.dragBy(d.dx, d.dy);
    },
    onDragEnd: _ed.endDrag,
    onPortDragStart: (port, g) {
      _focus.requestFocus();
      setState(() {
        _pendingFrom = port;
        _pendingStartGlobal = g;
        _pendingPos = _globalToCanvas(g);
      });
    },
    onPortDragUpdate: (g) => setState(() => _pendingPos = _globalToCanvas(g)),
    onPortDragEnd: _finishConnect,
  );

  void _finishConnect(Offset global) {
    final from = _pendingFrom;
    if (from == null) return;
    final pos = _globalToCanvas(global);
    final moved = (global - _pendingStartGlobal).distance;
    setState(() {
      _pendingFrom = null;
      _pendingPos = null;
    });
    final layouts = _layouts(_ed.workflow);
    PortSlot? target;
    // Wider snap for fingers (their drop point is imprecise and hidden).
    var best = isTouchLayout(context) ? 40.0 : 24.0;
    for (final l in layouts.values) {
      if (l.node.name == from.node) continue;
      for (final p in from.output ? l.inputs : l.outputs) {
        if (p.ref.type != from.type) continue;
        final d = (p.anchor - pos).distance;
        if (d < best) {
          best = d;
          target = p;
        }
      }
    }
    if (target != null) {
      _connectPorts(from, target.ref);
    } else if (from.output && moved > 12) {
      widget.onAddNode((x: pos.dx, y: pos.dy - kNodeMinHeight / 2), from);
    }
  }

  void _connectPorts(PortRef from, PortRef t) {
    final edge = from.output
        ? FlowEdge(
            src: from.node,
            srcIndex: from.index,
            dst: t.node,
            dstIndex: t.index,
            type: from.type,
          )
        : FlowEdge(
            src: t.node,
            srcIndex: t.index,
            dst: from.node,
            dstIndex: from.index,
            type: from.type,
          );
    if (!_ed.connect(edge)) widget.onMessage('Already connected');
  }

  /// Tap-to-connect for touch: tap a port to arm it, tap a compatible port on
  /// another node to link; anything else re-arms or cancels. Dragging a port
  /// still works.
  void _onPortTap(PortRef port) {
    _focus.requestFocus();
    final from = _pendingFrom;
    if (from == null) {
      setState(() {
        _pendingFrom = port;
        _pendingPos = null;
      });
      return;
    }
    final valid =
        from.node != port.node &&
        from.output != port.output &&
        from.type == port.type;
    if (valid) _connectPorts(from, port);
    setState(() {
      _pendingFrom = valid || from == port ? null : port;
      _pendingPos = null;
    });
  }

  // keyboard

  KeyEventResult _onKey(FocusNode node, KeyEvent e) {
    if (e is KeyUpEvent) return KeyEventResult.ignored;
    final kb = HardwareKeyboard.instance;
    final mod = kb.isControlPressed || kb.isMetaPressed;
    final k = e.logicalKey;
    if (k == LogicalKeyboardKey.delete || k == LogicalKeyboardKey.backspace) {
      _ed.deleteSelection();
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.escape) {
      setState(() {
        _pendingFrom = null;
        _pendingPos = null;
      });
      _ed.clearSelection();
      return KeyEventResult.handled;
    }
    if (k == LogicalKeyboardKey.tab && !mod) {
      final c = viewCenter;
      widget.onAddNode(c, null);
      return KeyEventResult.handled;
    }
    if (!mod) return KeyEventResult.ignored;
    if (k == LogicalKeyboardKey.keyZ) {
      kb.isShiftPressed ? _ed.redo() : _ed.undo();
    } else if (k == LogicalKeyboardKey.keyY) {
      _ed.redo();
    } else if (k == LogicalKeyboardKey.keyA) {
      _ed.selectAll();
    } else if (k == LogicalKeyboardKey.keyS) {
      widget.onSave();
    } else if (k == LogicalKeyboardKey.keyC) {
      copy();
    } else if (k == LogicalKeyboardKey.keyX) {
      copy(cut: true);
    } else if (k == LogicalKeyboardKey.keyV) {
      pasteFromClipboard();
    } else {
      return KeyEventResult.ignored;
    }
    return KeyEventResult.handled;
  }

  Future<void> copy({bool cut = false}) async {
    final s = cut ? _ed.cutSelection() : _ed.copySelection();
    if (s == null) return;
    await Clipboard.setData(ClipboardData(text: s));
    widget.onMessage(
      '${cut ? 'Cut' : 'Copied'} ${_ed.selection.length} node(s) as n8n JSON',
    );
  }

  Future<void> pasteFromClipboard() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    final text = data?.text;
    if (text == null || text.trim().isEmpty) return;
    try {
      final names = _ed.paste(text);
      if (names.isEmpty) {
        widget.onMessage('Clipboard has no n8n nodes');
      } else {
        widget.onMessage('Pasted ${names.length} node(s)');
      }
    } on FlowFormatException catch (e) {
      widget.onMessage('Paste failed: ${e.message}');
    }
  }

  // build

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return LayoutBuilder(
      builder: (context, c) {
        _viewport = c.biggest;
        return ListenableBuilder(
          listenable: _ed,
          builder: (context, _) {
            final wf = _ed.workflow;
            final layouts = _layouts(wf);
            final sel = _ed.selection;
            final selEdge = _ed.selectedEdge;
            final selEnds = selEdge == null ? null : edgeEnds(selEdge, layouts);

            final nodes = <Widget>[
              for (final l in layouts.values)
                Positioned(
                  key: ValueKey(l.node.name),
                  left: l.rect.left - kPortPad + _origin,
                  top: l.rect.top - kPortPad + _origin,
                  child: NodeCard(
                    layout: l,
                    selected: sel.contains(l.node.name),
                    callbacks: _callbacks(l.node.name),
                    pendingFrom: _pendingFrom,
                  ),
                ),
              if (selEnds != null) _edgeHandle(selEdge!, selEnds, hk),
            ];

            return Focus(
              focusNode: _focus,
              onKeyEvent: _onKey,
              child: Listener(
                onPointerSignal: _onPointerSignal,
                onPointerPanZoomStart: (_) => _pzScale = 1,
                onPointerPanZoomUpdate: (e) {
                  final f = e.scale / _pzScale;
                  _pzScale = e.scale;
                  setState(() => _pan += e.panDelta);
                  if (f != 1) _zoomAt(e.localPosition, f);
                },
                child: ClipRect(
                  child: Stack(
                    children: [
                      Positioned.fill(
                        child: GestureDetector(
                          behavior: HitTestBehavior.opaque,
                          onScaleStart: _onScaleStart,
                          onScaleUpdate: _onScaleUpdate,
                          onScaleEnd: _onScaleEnd,
                          onTapUp: _onTapUp,
                          onLongPressStart: (d) {
                            final p = _toCanvas(d.localPosition);
                            widget.onAddNode((x: p.dx, y: p.dy), null);
                          },
                          child: CustomPaint(
                            size: Size.infinite,
                            painter: _CanvasPainter(
                              hk: hk,
                              pan: _pan,
                              scale: _scale,
                              layouts: layouts,
                              edges: wf.connections.edges.toList(),
                              selected: selEdge,
                              pendingFrom: _pendingFrom,
                              pendingPos: _pendingPos,
                              marquee: _marqueeA == null || _marqueeB == null
                                  ? null
                                  : Rect.fromPoints(_marqueeA!, _marqueeB!),
                            ),
                          ),
                        ),
                      ),
                      Positioned.fill(
                        child: OverflowBox(
                          alignment: Alignment.topLeft,
                          minWidth: 0,
                          minHeight: 0,
                          maxWidth: _origin * 2,
                          maxHeight: _origin * 2,
                          child: Transform(
                            transform: Matrix4.identity()
                              ..translateByDouble(
                                _pan.dx - _origin * _scale,
                                _pan.dy - _origin * _scale,
                                0,
                                1,
                              )
                              ..scaleByDouble(_scale, _scale, 1, 1),
                            child: SizedBox(
                              width: _origin * 2,
                              height: _origin * 2,
                              child: Stack(
                                clipBehavior: Clip.none,
                                children: nodes,
                              ),
                            ),
                          ),
                        ),
                      ),
                      Positioned(
                        left: HkTokens.s3,
                        bottom: HkTokens.s3,
                        child: _ZoomBar(
                          scale: _scale,
                          onZoom: setZoom,
                          onFit: fit,
                        ),
                      ),
                    ],
                  ),
                ),
              ),
            );
          },
        );
      },
    );
  }

  /// "x" button on the middle of the selected edge (touch-friendly delete).
  Widget _edgeHandle(
    FlowEdge e,
    ({Offset a, PortSide aSide, Offset b, PortSide bSide}) ends,
    HkTokens hk,
  ) {
    final pts = edgePoints(ends.a, ends.aSide, ends.b, ends.bSide);
    final mid = pts[pts.length ~/ 2];
    return Positioned(
      left: mid.dx - 12 + _origin,
      top: mid.dy - 12 + _origin,
      width: 24,
      height: 24,
      child: Tooltip(
        message: 'Delete connection',
        child: GestureDetector(
          onTap: () => _ed.deleteSelection(),
          child: Container(
            decoration: BoxDecoration(
              color: hk.surface,
              shape: BoxShape.circle,
              border: Border.all(color: hk.accent, width: 2),
            ),
            child: Icon(Icons.close, size: 14, color: hk.accent),
          ),
        ),
      ),
    );
  }
}

class _ZoomBar extends StatelessWidget {
  const _ZoomBar({
    required this.scale,
    required this.onZoom,
    required this.onFit,
  });

  final double scale;
  final void Function(double) onZoom;
  final VoidCallback onFit;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Material(
      color: hk.surface,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        side: BorderSide(color: hk.border),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          IconButton(
            tooltip: 'Zoom out',
            icon: const Icon(Icons.remove, size: 18),
            onPressed: () => onZoom(scale / 1.25),
          ),
          // Pinch zooms on touch; the slider is for pointer users.
          if (!isNarrow(context))
            SizedBox(
              width: 110,
              child: Slider(
                value: scale.clamp(_minScale, _maxScale),
                min: _minScale,
                max: _maxScale,
                onChanged: onZoom,
              ),
            ),
          IconButton(
            tooltip: 'Zoom in',
            icon: const Icon(Icons.add, size: 18),
            onPressed: () => onZoom(scale * 1.25),
          ),
          IconButton(
            tooltip: 'Fit to content',
            icon: const Icon(Icons.fit_screen, size: 18),
            onPressed: onFit,
          ),
        ],
      ),
    );
  }
}

class _CanvasPainter extends CustomPainter {
  _CanvasPainter({
    required this.hk,
    required this.pan,
    required this.scale,
    required this.layouts,
    required this.edges,
    required this.selected,
    required this.pendingFrom,
    required this.pendingPos,
    required this.marquee,
  });

  final HkTokens hk;
  final Offset pan;
  final double scale;
  final Map<String, NodeLayout> layouts;
  final List<FlowEdge> edges;
  final FlowEdge? selected;
  final PortRef? pendingFrom;
  final Offset? pendingPos;
  final Rect? marquee;

  @override
  void paint(Canvas canvas, Size size) {
    // dot grid in screen space
    var step = 24 * scale;
    if (step < 12) step *= 4;
    final dot = Paint()..color = hk.border;
    final ox = pan.dx % step;
    final oy = pan.dy % step;
    for (var x = ox; x < size.width; x += step) {
      for (var y = oy; y < size.height; y += step) {
        canvas.drawCircle(Offset(x, y), 1, dot);
      }
    }

    canvas
      ..save()
      ..translate(pan.dx, pan.dy)
      ..scale(scale);

    for (final e in edges) {
      final ends = edgeEnds(e, layouts);
      if (ends == null) continue;
      final sel = e == selected;
      final p = Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = sel ? 3 : 2
        ..strokeCap = StrokeCap.round
        ..color = sel ? hk.accent : hk.textMuted;
      canvas.drawPath(edgePath(ends.a, ends.aSide, ends.b, ends.bSide), p);
      _arrow(canvas, ends.b, ends.bSide, p..style = PaintingStyle.fill);
    }

    final from = pendingFrom;
    final to = pendingPos;
    if (from != null && to != null) {
      final l = layouts[from.node];
      final slot = from.output
          ? l?.output(from.type, from.index)
          : l?.input(from.type, from.index);
      if (slot != null) {
        final p = Paint()
          ..style = PaintingStyle.stroke
          ..strokeWidth = 2
          ..color = hk.accent;
        final toSide = from.output ? PortSide.left : PortSide.right;
        canvas.drawPath(edgePath(slot.anchor, slot.side, to, toSide), p);
      }
    }

    final m = marquee;
    if (m != null) {
      canvas
        ..drawRect(m, Paint()..color = hk.accent.withValues(alpha: 0.12))
        ..drawRect(
          m,
          Paint()
            ..style = PaintingStyle.stroke
            ..color = hk.accent,
        );
    }
    canvas.restore();
  }

  void _arrow(Canvas canvas, Offset tip, PortSide side, Paint paint) {
    // Points into the port: a left input is approached moving right.
    final dir = switch (side) {
      PortSide.left => const Offset(1, 0),
      PortSide.right => const Offset(-1, 0),
      PortSide.top => const Offset(0, 1),
      PortSide.bottom => const Offset(0, -1),
    };
    final n = Offset(-dir.dy, dir.dx);
    final back = tip - dir * 9;
    canvas.drawPath(
      Path()
        ..moveTo(tip.dx, tip.dy)
        ..lineTo(back.dx + n.dx * 4.5, back.dy + n.dy * 4.5)
        ..lineTo(back.dx - n.dx * 4.5, back.dy - n.dy * 4.5)
        ..close(),
      paint,
    );
  }

  @override
  bool shouldRepaint(_CanvasPainter old) => true;
}
