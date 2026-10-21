import 'package:flutter/material.dart';

import 'tokens.dart';

ThemeData buildTheme(Brightness brightness) {
  final t = brightness == Brightness.dark ? HkTokens.dark : HkTokens.light;
  final scheme =
      ColorScheme.fromSeed(
        seedColor: t.accent,
        brightness: brightness,
      ).copyWith(
        primary: t.accent,
        onPrimary: const Color(0xFF17181C),
        secondary: t.accent2,
        surface: t.surface,
        onSurface: t.text,
        onSurfaceVariant: t.textMuted,
        outline: t.border,
        outlineVariant: t.border,
        error: t.err,
      );

  return ThemeData(
    useMaterial3: true,
    brightness: brightness,
    colorScheme: scheme,
    scaffoldBackgroundColor: t.bg,
    canvasColor: t.bg,
    fontFamily: HkTokens.fontUi,
    dividerColor: t.border,
    extensions: [t],
    textTheme: TextTheme(
      bodySmall: TextStyle(fontSize: 12, color: t.textMuted),
      bodyMedium: TextStyle(fontSize: 14, color: t.text),
      bodyLarge: TextStyle(fontSize: 16, color: t.text),
      titleMedium: TextStyle(
        fontSize: 16,
        fontWeight: FontWeight.w600,
        color: t.text,
      ),
      titleLarge: TextStyle(
        fontSize: 20,
        fontWeight: FontWeight.w600,
        color: t.text,
      ),
      headlineSmall: TextStyle(
        fontSize: 24,
        fontWeight: FontWeight.w600,
        color: t.text,
      ),
    ),
    cardTheme: CardThemeData(
      color: t.surface,
      elevation: 0,
      margin: EdgeInsets.zero,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        side: BorderSide(color: t.border),
      ),
    ),
    navigationRailTheme: NavigationRailThemeData(
      backgroundColor: t.surface,
      indicatorColor: t.accent.withValues(alpha: 0.18),
      selectedIconTheme: IconThemeData(color: t.accent),
      unselectedIconTheme: IconThemeData(color: t.textMuted),
      selectedLabelTextStyle: TextStyle(fontSize: 12, color: t.text),
      unselectedLabelTextStyle: TextStyle(fontSize: 12, color: t.textMuted),
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: t.surface,
      indicatorColor: t.accent.withValues(alpha: 0.18),
      labelTextStyle: WidgetStatePropertyAll(
        TextStyle(fontSize: 12, color: t.text),
      ),
    ),
    dividerTheme: DividerThemeData(color: t.border, space: 1, thickness: 1),
    visualDensity: VisualDensity.standard,
  );
}
