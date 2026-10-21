import 'package:flutter/material.dart';

import '../theme/tokens.dart';
import 'breakpoints.dart';

/// Dialog on wide windows, bottom sheet on narrow ones. [builder] returns bare
/// content (no `Dialog` / `AlertDialog`); the wrapper supplies the chrome, the
/// keyboard inset and the safe area.
///
/// [tall] sheets fill most of the screen for content that scrolls on its own
/// (lists, diffs); otherwise the sheet hugs its content.
Future<T?> showAdaptiveSheetOrDialog<T>(
  BuildContext context, {
  required WidgetBuilder builder,
  double maxWidth = 480,
  double maxHeight = 560,
  bool tall = false,
}) {
  if (!isNarrow(context)) {
    return showDialog<T>(
      context: context,
      builder: (ctx) => Dialog(
        child: ConstrainedBox(
          constraints: BoxConstraints(maxWidth: maxWidth, maxHeight: maxHeight),
          child: builder(ctx),
        ),
      ),
    );
  }
  return showModalBottomSheet<T>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    showDragHandle: true,
    shape: const RoundedRectangleBorder(
      borderRadius: BorderRadius.vertical(
        top: Radius.circular(HkTokens.radiusSheet),
      ),
    ),
    builder: (ctx) {
      // Read from the sheet's own context so the keyboard inset stays live.
      final inset = MediaQuery.viewInsetsOf(ctx).bottom;
      final h = MediaQuery.sizeOf(ctx).height;
      final content = builder(ctx);
      return Padding(
        padding: EdgeInsets.only(bottom: inset),
        child: tall
            ? SizedBox(height: (h * 0.85 - inset).clamp(200, h), child: content)
            : ConstrainedBox(
                constraints: BoxConstraints(maxHeight: h * 0.85),
                child: content,
              ),
      );
    },
  );
}
