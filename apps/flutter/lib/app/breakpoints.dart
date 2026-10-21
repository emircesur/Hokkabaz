import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

/// Below this width: bottom nav, list -> detail navigation, sheets instead of
/// dialogs (DESIGN §4). At or above: nav rail + multi-pane.
const double kRailBreakpoint = 720;

/// Chat: sessions and conversation side by side.
const double kChatTwoPaneMin = kRailBreakpoint;

/// Chat: inspector becomes a third column.
const double kChatInspectorMin = 1100;

/// Flows: list + canvas + node panel columns; narrower is canvas-only with
/// pickers and params in bottom sheets.
const double kFlowsWideMin = 900;

/// Mobile comfortable density (DESIGN §4.7).
const double kTouchTarget = 48;

bool isNarrowWidth(double width) => width < kRailBreakpoint;

bool isNarrow(BuildContext context) =>
    isNarrowWidth(MediaQuery.sizeOf(context).width);

/// Minimum row height for tappable rows: 48 on narrow, free on desktop.
double touchMinHeight(BuildContext context) =>
    isNarrow(context) ? kTouchTarget : 0;

/// Finger-driven: phone/tablet OS or a narrow window. Gates bigger hit boxes.
bool isTouchLayout(BuildContext context) =>
    isNarrow(context) ||
    defaultTargetPlatform == TargetPlatform.android ||
    defaultTargetPlatform == TargetPlatform.iOS;
