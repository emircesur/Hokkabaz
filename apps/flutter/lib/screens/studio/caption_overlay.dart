import 'package:flutter/foundation.dart' show ValueListenable;
import 'package:flutter/material.dart';

import 'studio_models.dart';

Color hexColor(String? hex, Color fallback) {
  final h = (hex ?? '').replaceFirst('#', '');
  if (h.length != 6) return fallback;
  final v = int.tryParse(h, radix: 16);
  return v == null ? fallback : Color(0xFF000000 | v);
}

/// Lines of caption words, grouped like the renderer does (pauses, word and
/// character limits, sentence ends) so the preview matches the burned-in result.
List<List<CaptionWord>> groupCaptionLines(List<CaptionWord> words, Map<String, dynamic>? style) {
  final maxWords = (style?['max_words'] as num?)?.toInt() ?? 7;
  final maxChars = (style?['max_chars'] as num?)?.toInt() ?? 38;
  final maxGap = (style?['max_gap'] as num?)?.toDouble() ?? 0.8;
  final lines = <List<CaptionWord>>[];
  var cur = <CaptionWord>[];
  var chars = 0;
  for (final w in words) {
    if (cur.isNotEmpty) {
      final p = cur.last;
      final breaks = w.start - p.end > maxGap ||
          cur.length >= (maxWords < 1 ? 1 : maxWords) ||
          chars + 1 + w.text.runes.length > (maxChars < 1 ? 1 : maxChars) ||
          p.text.endsWith('.') ||
          p.text.endsWith('!') ||
          p.text.endsWith('?');
      if (breaks) {
        lines.add(cur);
        cur = [];
        chars = 0;
      }
    }
    chars += w.text.runes.length + (cur.isEmpty ? 0 : 1);
    cur.add(w);
  }
  if (cur.isNotEmpty) lines.add(cur);
  return lines;
}

/// What to show at output time [t]: the line and which word is active.
({List<CaptionWord> line, int active})? captionAt(List<List<CaptionWord>> lines, double t) {
  for (final l in lines) {
    final start = l.first.start;
    final end = l.last.end;
    if (t >= start && t < end + 0.15) {
      var active = -1;
      for (var i = 0; i < l.length; i++) {
        if (t >= l[i].start && t < l[i].end) active = i;
        if (t >= l[i].start) active = active == -1 ? i : active;
      }
      return (line: l, active: active);
    }
  }
  return null;
}

/// Burned-in caption preview, positioned and styled like the render.
class CaptionOverlay extends StatelessWidget {
  const CaptionOverlay({super.key, required this.lines, required this.style, required this.time});

  final List<List<CaptionWord>> lines;
  final Map<String, dynamic>? style;
  final ValueListenable<double> time;

  @override
  Widget build(BuildContext context) {
    final st = style;
    if (st == null) return const SizedBox.shrink();
    return LayoutBuilder(
      builder: (context, box) {
        final scale = box.maxHeight / 1080.0;
        final size = ((st['size'] as num?)?.toDouble() ?? 56) * scale;
        final primary = hexColor(st['primary'] as String?, Colors.white);
        final highlight = hexColor(st['highlight'] as String?, primary);
        final outline = hexColor(st['outline_color'] as String?, Colors.black);
        final mode = st['mode'] as String? ?? 'block';
        final upper = st['uppercase'] == true;
        final bold = st['bold'] != false;
        final pos = st['pos'] as String? ?? 'bottom';
        final margin = ((st['margin_v'] as num?)?.toDouble() ?? 0) * scale;
        final align = switch (pos) {
          'top' => Alignment.topCenter,
          'center' => Alignment.center,
          _ => Alignment.bottomCenter,
        };
        return ValueListenableBuilder<double>(
          valueListenable: time,
          builder: (context, t, _) {
            final hit = captionAt(lines, t);
            if (hit == null) return const SizedBox.shrink();
            String tx(String s) => upper ? s.toUpperCase() : s;
            TextStyle base(Color c) => TextStyle(
              fontSize: size.clamp(8, 400),
              fontWeight: bold ? FontWeight.w800 : FontWeight.w500,
              color: c,
              shadows: [for (final o in const [Offset(1, 1), Offset(-1, -1), Offset(1, -1), Offset(-1, 1)]) Shadow(color: outline, offset: o * (size / 28).clamp(1, 4))],
            );
            final spans = <InlineSpan>[];
            if (mode == 'word_by_word') {
              final w = hit.line[hit.active.clamp(0, hit.line.length - 1)];
              spans.add(TextSpan(text: tx(w.text), style: base(highlight)));
            } else {
              for (var i = 0; i < hit.line.length; i++) {
                final on = mode == 'karaoke' && i == hit.active;
                spans.add(TextSpan(text: '${i == 0 ? '' : ' '}${tx(hit.line[i].text)}', style: base(on ? highlight : primary)));
              }
            }
            return Align(
              alignment: align,
              child: Padding(
                padding: EdgeInsets.only(left: box.maxWidth * 0.06, right: box.maxWidth * 0.06, bottom: pos == 'bottom' ? margin : 0, top: pos == 'top' ? margin : 0),
                child: Text.rich(TextSpan(children: spans), textAlign: TextAlign.center),
              ),
            );
          },
        );
      },
    );
  }
}


