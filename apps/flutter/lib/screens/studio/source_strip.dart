import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'studio_models.dart';

/// Whole source video as one bar: clips are highlighted, the selected clip has
/// draggable edges, the playhead shows where playback is. Long-press empty
/// space to add a clip there.
class SourceStrip extends StatefulWidget {
  const SourceStrip({
    super.key,
    required this.duration,
    required this.clips,
    required this.selected,
    required this.playhead,
    required this.onSelect,
    required this.onBounds,
    required this.onAddAt,
  });

  final double duration;
  final List<StudioClip> clips;
  final String? selected;
  final ValueNotifier<double> playhead;
  final void Function(String id) onSelect;
  final void Function(String id, double start, double end) onBounds;
  final void Function(double at) onAddAt;

  @override
  State<SourceStrip> createState() => _SourceStripState();
}

enum _Edge { start, end }

class _SourceStripState extends State<SourceStrip> {
  _Edge? _drag;
  double? _dragStart;
  double? _dragEnd;
  double _doubleAt = 0;
  bool _overEdge = false;

  double _t(double dx, double w) =>
      (dx / w * widget.duration).clamp(0.0, widget.duration);

  StudioClip? get _selClip =>
      widget.clips.where((c) => c.id == widget.selected).firstOrNull;

  _Edge? _edgeAt(double dx, double w) {
    final c = _selClip;
    if (c == null || widget.duration == 0) return null;
    final xs = c.start / widget.duration * w;
    final xe = c.end / widget.duration * w;
    if ((dx - xs).abs() <= 14) return _Edge.start;
    if ((dx - xe).abs() <= 14) return _Edge.end;
    return null;
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return LayoutBuilder(
      builder: (context, box) {
        final w = box.maxWidth;
        final strip = GestureDetector(
          behavior: HitTestBehavior.opaque,
          onDoubleTapDown: (d) => _doubleAt = d.localPosition.dx,
          onDoubleTap: () => widget.onAddAt(_t(_doubleAt, w)),
          onTapUp: (d) {
            final t = _t(d.localPosition.dx, w);
            final hit = widget.clips
                .where((c) => t >= c.start && t <= c.end)
                .firstOrNull;
            if (hit != null) widget.onSelect(hit.id);
          },
          onLongPressStart: (d) => widget.onAddAt(_t(d.localPosition.dx, w)),
          onPanStart: (d) {
            final e = _edgeAt(d.localPosition.dx, w);
            final c = _selClip;
            if (e != null && c != null) {
              setState(() {
                _drag = e;
                _dragStart = c.start;
                _dragEnd = c.end;
              });
            }
          },
          onPanUpdate: (d) {
            if (_drag == null) return;
            final t = _t(d.localPosition.dx, w);
            setState(() {
              if (_drag == _Edge.start) {
                _dragStart = t.clamp(0.0, (_dragEnd ?? t) - 1.0);
              } else {
                _dragEnd = t.clamp((_dragStart ?? t) + 1.0, widget.duration);
              }
            });
          },
          onPanEnd: (_) {
            final c = _selClip;
            if (_drag != null &&
                c != null &&
                _dragStart != null &&
                _dragEnd != null) {
              widget.onBounds(c.id, _dragStart!, _dragEnd!);
            }
            setState(() {
              _drag = null;
              _dragStart = null;
              _dragEnd = null;
            });
          },
          child: SizedBox(
            height: 44,
            child: ValueListenableBuilder<double>(
              valueListenable: widget.playhead,
              builder: (context, ph, _) => CustomPaint(
                size: Size(w, 44),
                painter: _StripPainter(
                  duration: widget.duration,
                  clips: widget.clips,
                  selected: widget.selected,
                  playhead: ph,
                  dragStart: _dragStart,
                  dragEnd: _dragEnd,
                  track: hk.border,
                  clipColor: hk.accent2,
                  selectedColor: hk.accent,
                  head: hk.text,
                ),
              ),
            ),
          ),
        );
        return Tooltip(
          message: 'Whole source video. Drag the selected clip\'s edges to trim; double-click or long-press empty space to add a clip.',
          waitDuration: const Duration(milliseconds: 900),
          child: MouseRegion(
            cursor: _overEdge || _drag != null
                ? SystemMouseCursors.resizeLeftRight
                : SystemMouseCursors.click,
            onHover: (e) {
              final over = _edgeAt(e.localPosition.dx, w) != null;
              if (over != _overEdge) setState(() => _overEdge = over);
            },
            child: strip,
          ),
        );
      },
    );
  }
}

class _StripPainter extends CustomPainter {
  _StripPainter({
    required this.duration,
    required this.clips,
    required this.selected,
    required this.playhead,
    required this.dragStart,
    required this.dragEnd,
    required this.track,
    required this.clipColor,
    required this.selectedColor,
    required this.head,
  });

  final double duration;
  final List<StudioClip> clips;
  final String? selected;
  final double playhead;
  final double? dragStart;
  final double? dragEnd;
  final Color track;
  final Color clipColor;
  final Color selectedColor;
  final Color head;

  @override
  void paint(Canvas canvas, Size size) {
    if (duration <= 0) return;
    final y0 = size.height * 0.25;
    final h = size.height * 0.5;
    canvas.drawRRect(
      RRect.fromRectAndRadius(
        Rect.fromLTWH(0, y0, size.width, h),
        const Radius.circular(4),
      ),
      Paint()..color = track,
    );
    double x(double t) => t / duration * size.width;
    for (final c in clips) {
      final isSel = c.id == selected;
      final s = isSel && dragStart != null ? dragStart! : c.start;
      final e = isSel && dragEnd != null ? dragEnd! : c.end;
      final r = Rect.fromLTRB(
        x(s),
        y0,
        x(e).clamp(x(s) + 2, size.width),
        y0 + h,
      );
      canvas.drawRRect(
        RRect.fromRectAndRadius(r, const Radius.circular(4)),
        Paint()
          ..color = (isSel ? selectedColor : clipColor).withValues(
            alpha: isSel ? 0.85 : 0.5,
          ),
      );
      if (isSel) {
        final handle = Paint()..color = head;
        canvas.drawRect(Rect.fromLTWH(r.left - 1.5, y0 - 4, 3, h + 8), handle);
        canvas.drawRect(Rect.fromLTWH(r.right - 1.5, y0 - 4, 3, h + 8), handle);
      }
    }
    final ph = x(playhead.clamp(0.0, duration));
    canvas.drawLine(
      Offset(ph, 0),
      Offset(ph, size.height),
      Paint()
        ..color = head
        ..strokeWidth = 2,
    );
  }

  @override
  bool shouldRepaint(_StripPainter old) =>
      old.playhead != playhead ||
      old.selected != selected ||
      old.clips != clips ||
      old.dragStart != dragStart ||
      old.dragEnd != dragEnd ||
      old.duration != duration;
}
