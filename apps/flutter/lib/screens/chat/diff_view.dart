import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'chat_model.dart';

/// Unified-style diff rows, monospace, red/green tinted.
class DiffView extends StatelessWidget {
  const DiffView({super.key, required this.lines, this.maxLines = 200});

  final List<DiffLine> lines;
  final int maxLines;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final shown = lines.length > maxLines ? lines.sublist(0, maxLines) : lines;
    return Container(
      width: double.infinity,
      decoration: BoxDecoration(
        color: hk.bg,
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (final l in shown) _row(hk, l),
            if (lines.length > shown.length)
              Padding(
                padding: const EdgeInsets.all(HkTokens.s2),
                child: Text(
                  '… ${lines.length - shown.length} more lines',
                  style: TextStyle(color: hk.textMuted, fontSize: 12),
                ),
              ),
          ],
        ),
      ),
    );
  }

  Widget _row(HkTokens hk, DiffLine l) {
    final (bg, fg, sign) = switch (l.kind) {
      DiffKind.add => (hk.ok.withValues(alpha: 0.14), hk.ok, '+'),
      DiffKind.remove => (hk.err.withValues(alpha: 0.14), hk.err, '-'),
      DiffKind.context => (Colors.transparent, hk.textMuted, ' '),
    };
    return Container(
      color: bg,
      padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2, vertical: 1),
      child: Text(
        '$sign ${l.text}',
        style: TextStyle(
          fontFamily: HkTokens.fontMono,
          fontSize: 12,
          height: 1.35,
          color: fg,
        ),
      ),
    );
  }
}
