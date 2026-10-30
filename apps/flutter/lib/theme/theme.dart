import 'package:flutter/material.dart';

import 'tokens.dart';

/// App theme: compact pro-tool density, every Material component mapped onto [HkTokens] so
/// screens get the look without per-widget styling.
ThemeData buildTheme(Brightness brightness) {
  final t = brightness == Brightness.dark ? HkTokens.dark : HkTokens.light;
  final scheme =
      ColorScheme.fromSeed(
        seedColor: t.accent,
        brightness: brightness,
      ).copyWith(
        primary: t.accent,
        onPrimary: t.onAccent,
        primaryContainer: t.accent.withValues(alpha: 0.18),
        onPrimaryContainer: t.text,
        secondary: t.accent2,
        onSecondary: t.onAccent,
        secondaryContainer: t.surface3,
        onSecondaryContainer: t.text,
        tertiary: t.info,
        surface: t.surface,
        onSurface: t.text,
        onSurfaceVariant: t.textMuted,
        surfaceContainerLowest: t.bg,
        surfaceContainerLow: t.surface,
        surfaceContainer: t.surface2,
        surfaceContainerHigh: t.surface2,
        surfaceContainerHighest: t.surface3,
        outline: t.borderStrong,
        outlineVariant: t.border,
        error: t.err,
        surfaceTint: Colors.transparent,
      );

  final ctl = RoundedRectangleBorder(
    borderRadius: BorderRadius.circular(HkTokens.radiusControl),
  );
  const pad = EdgeInsets.symmetric(horizontal: 14, vertical: 10);
  const btnText = TextStyle(
    fontFamily: HkTokens.fontUi,
    fontSize: 13,
    fontWeight: FontWeight.w600,
    letterSpacing: 0.1,
  );
  OutlineInputBorder field(Color c, [double w = 1]) => OutlineInputBorder(
    borderRadius: BorderRadius.circular(HkTokens.radiusControl),
    borderSide: BorderSide(color: c, width: w),
  );

  final text = TextTheme(
    displaySmall: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 30,
      fontWeight: FontWeight.w700,
      letterSpacing: -0.6,
      color: t.text,
    ),
    headlineMedium: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 24,
      fontWeight: FontWeight.w700,
      letterSpacing: -0.4,
      color: t.text,
    ),
    headlineSmall: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 20,
      fontWeight: FontWeight.w700,
      letterSpacing: -0.3,
      color: t.text,
    ),
    titleLarge: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 17,
      fontWeight: FontWeight.w600,
      letterSpacing: -0.2,
      color: t.text,
    ),
    titleMedium: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 14,
      fontWeight: FontWeight.w600,
      color: t.text,
    ),
    titleSmall: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 13,
      fontWeight: FontWeight.w600,
      color: t.text,
    ),
    bodyLarge: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 14,
      height: 1.5,
      color: t.text,
    ),
    bodyMedium: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 13,
      height: 1.45,
      color: t.text,
    ),
    bodySmall: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 12,
      height: 1.4,
      color: t.textMuted,
    ),
    labelLarge: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 13,
      fontWeight: FontWeight.w600,
      color: t.text,
    ),
    labelMedium: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 12,
      fontWeight: FontWeight.w500,
      color: t.textMuted,
    ),
    labelSmall: TextStyle(
      fontFamily: HkTokens.fontUi,
      fontSize: 11,
      fontWeight: FontWeight.w600,
      letterSpacing: 0.6,
      color: t.textFaint,
    ),
  );

  return ThemeData(
    useMaterial3: true,
    brightness: brightness,
    colorScheme: scheme,
    scaffoldBackgroundColor: t.bg,
    canvasColor: t.surface,
    fontFamily: HkTokens.fontUi,
    dividerColor: t.border,
    hoverColor: t.surface3.withValues(alpha: 0.6),
    highlightColor: Colors.transparent,
    splashFactory: InkSparkle.splashFactory,
    focusColor: t.accent.withValues(alpha: 0.14),
    extensions: [t],
    textTheme: text,
    // Compact on desktop, standard (48dp targets) on phones.
    visualDensity: VisualDensity.adaptivePlatformDensity,
    materialTapTargetSize: MaterialTapTargetSize.padded,
    iconTheme: IconThemeData(color: t.textMuted, size: 20),
    appBarTheme: AppBarTheme(
      backgroundColor: t.surface,
      foregroundColor: t.text,
      elevation: 0,
      scrolledUnderElevation: 0,
      centerTitle: false,
      toolbarHeight: 52,
      titleTextStyle: text.titleLarge,
      shape: Border(bottom: BorderSide(color: t.border)),
    ),
    cardTheme: CardThemeData(
      color: t.surface2,
      elevation: 0,
      margin: EdgeInsets.zero,
      clipBehavior: Clip.antiAlias,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        side: BorderSide(color: t.border),
      ),
    ),
    filledButtonTheme: FilledButtonThemeData(
      style: FilledButton.styleFrom(
        backgroundColor: t.accent,
        foregroundColor: t.onAccent,
        disabledBackgroundColor: t.surface3,
        disabledForegroundColor: t.textFaint,
        padding: pad,
        minimumSize: const Size(0, 36),
        shape: ctl,
        textStyle: btnText,
        elevation: 0,
      ),
    ),
    elevatedButtonTheme: ElevatedButtonThemeData(
      style: ElevatedButton.styleFrom(
        backgroundColor: t.surface3,
        foregroundColor: t.text,
        padding: pad,
        minimumSize: const Size(0, 36),
        shape: ctl,
        textStyle: btnText,
        elevation: 0,
      ),
    ),
    outlinedButtonTheme: OutlinedButtonThemeData(
      style: OutlinedButton.styleFrom(
        foregroundColor: t.text,
        padding: pad,
        minimumSize: const Size(0, 36),
        shape: ctl,
        side: BorderSide(color: t.borderStrong),
        textStyle: btnText,
      ),
    ),
    textButtonTheme: TextButtonThemeData(
      style: TextButton.styleFrom(
        foregroundColor: t.accent,
        padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
        minimumSize: const Size(0, 32),
        shape: ctl,
        textStyle: btnText,
      ),
    ),
    iconButtonTheme: IconButtonThemeData(
      style: IconButton.styleFrom(
        foregroundColor: t.textMuted,
        hoverColor: t.surface3,
        shape: ctl,
        minimumSize: const Size(34, 34),
        padding: const EdgeInsets.all(7),
        iconSize: 19,
      ),
    ),
    segmentedButtonTheme: SegmentedButtonThemeData(
      style: ButtonStyle(
        visualDensity: VisualDensity.compact,
        textStyle: const WidgetStatePropertyAll(
          TextStyle(
            fontFamily: HkTokens.fontUi,
            fontSize: 12,
            fontWeight: FontWeight.w600,
          ),
        ),
        side: WidgetStatePropertyAll(BorderSide(color: t.border)),
        shape: WidgetStatePropertyAll(ctl),
        backgroundColor: WidgetStateProperty.resolveWith(
          (s) => s.contains(WidgetState.selected) ? t.surface3 : t.surface2,
        ),
        foregroundColor: WidgetStateProperty.resolveWith(
          (s) => s.contains(WidgetState.selected) ? t.text : t.textMuted,
        ),
      ),
    ),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: t.surface2,
      isDense: true,
      contentPadding: const EdgeInsets.symmetric(horizontal: 12, vertical: 11),
      hintStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.textFaint,
      ),
      labelStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.textMuted,
      ),
      floatingLabelStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 12,
        color: t.accent,
      ),
      helperStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 11,
        color: t.textFaint,
      ),
      prefixIconColor: t.textMuted,
      suffixIconColor: t.textMuted,
      border: field(t.border),
      enabledBorder: field(t.border),
      focusedBorder: field(t.accent, 1.4),
      errorBorder: field(t.err),
      focusedErrorBorder: field(t.err, 1.4),
      disabledBorder: field(t.border.withValues(alpha: 0.5)),
    ),
    dropdownMenuTheme: DropdownMenuThemeData(
      textStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.text,
      ),
      menuStyle: MenuStyle(
        backgroundColor: WidgetStatePropertyAll(t.surface2),
        side: WidgetStatePropertyAll(BorderSide(color: t.border)),
        shape: WidgetStatePropertyAll(ctl),
      ),
    ),
    menuTheme: MenuThemeData(
      style: MenuStyle(
        backgroundColor: WidgetStatePropertyAll(t.surface2),
        side: WidgetStatePropertyAll(BorderSide(color: t.border)),
        shape: WidgetStatePropertyAll(ctl),
        elevation: const WidgetStatePropertyAll(8),
      ),
    ),
    popupMenuTheme: PopupMenuThemeData(
      color: t.surface2,
      elevation: 8,
      textStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.text,
      ),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        side: BorderSide(color: t.border),
      ),
    ),
    chipTheme: ChipThemeData(
      backgroundColor: t.surface2,
      selectedColor: t.accent.withValues(alpha: 0.2),
      disabledColor: t.surface,
      side: BorderSide(color: t.border),
      labelStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 12,
        fontWeight: FontWeight.w500,
        color: t.text,
      ),
      secondaryLabelStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 12,
        color: t.text,
      ),
      padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusChip),
      ),
      checkmarkColor: t.accent,
      iconTheme: IconThemeData(color: t.textMuted, size: 16),
    ),
    switchTheme: SwitchThemeData(
      thumbColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? t.onAccent : t.textMuted,
      ),
      trackColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? t.accent : t.surface3,
      ),
      trackOutlineColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected)
            ? Colors.transparent
            : t.borderStrong,
      ),
    ),
    checkboxTheme: CheckboxThemeData(
      fillColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? t.accent : Colors.transparent,
      ),
      checkColor: WidgetStatePropertyAll(t.onAccent),
      side: BorderSide(color: t.borderStrong, width: 1.4),
      shape: RoundedRectangleBorder(borderRadius: BorderRadius.circular(4)),
    ),
    radioTheme: RadioThemeData(
      fillColor: WidgetStateProperty.resolveWith(
        (s) => s.contains(WidgetState.selected) ? t.accent : t.borderStrong,
      ),
    ),
    sliderTheme: SliderThemeData(
      activeTrackColor: t.accent,
      inactiveTrackColor: t.surface3,
      thumbColor: t.onAccent,
      overlayColor: t.accent.withValues(alpha: 0.16),
      trackHeight: 3,
      thumbShape: const RoundSliderThumbShape(enabledThumbRadius: 7),
      valueIndicatorColor: t.surface3,
      valueIndicatorTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        color: t.text,
        fontSize: 12,
      ),
      showValueIndicator: ShowValueIndicator.onDrag,
      tickMarkShape: SliderTickMarkShape.noTickMark,
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(
      color: t.accent,
      linearTrackColor: t.surface3,
      circularTrackColor: Colors.transparent,
      linearMinHeight: 3,
      borderRadius: BorderRadius.circular(2),
    ),
    tabBarTheme: TabBarThemeData(
      labelColor: t.text,
      unselectedLabelColor: t.textMuted,
      labelStyle: const TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        fontWeight: FontWeight.w600,
      ),
      unselectedLabelStyle: const TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        fontWeight: FontWeight.w500,
      ),
      indicatorColor: t.accent,
      indicatorSize: TabBarIndicatorSize.label,
      dividerColor: t.border,
      overlayColor: WidgetStatePropertyAll(t.surface3.withValues(alpha: 0.5)),
    ),
    tooltipTheme: TooltipThemeData(
      decoration: BoxDecoration(
        color: t.surface3,
        borderRadius: BorderRadius.circular(6),
        border: Border.all(color: t.borderStrong),
      ),
      textStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 12,
        color: t.text,
      ),
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 5),
      waitDuration: const Duration(milliseconds: 400),
    ),
    dialogTheme: DialogThemeData(
      backgroundColor: t.surface,
      elevation: 24,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusSheet),
        side: BorderSide(color: t.border),
      ),
      titleTextStyle: text.titleLarge,
      contentTextStyle: text.bodyMedium,
    ),
    bottomSheetTheme: BottomSheetThemeData(
      backgroundColor: t.surface,
      modalBackgroundColor: t.surface,
      surfaceTintColor: Colors.transparent,
      showDragHandle: true,
      dragHandleColor: t.borderStrong,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(
          top: Radius.circular(HkTokens.radiusSheet),
        ),
      ),
    ),
    snackBarTheme: SnackBarThemeData(
      behavior: SnackBarBehavior.floating,
      backgroundColor: t.surface3,
      contentTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.text,
      ),
      actionTextColor: t.accent,
      elevation: 6,
      width: 440,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        side: BorderSide(color: t.borderStrong),
      ),
    ),
    bannerTheme: MaterialBannerThemeData(
      backgroundColor: t.surface2,
      contentTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        color: t.text,
      ),
    ),
    listTileTheme: ListTileThemeData(
      dense: true,
      iconColor: t.textMuted,
      textColor: t.text,
      selectedColor: t.text,
      selectedTileColor: t.surface3,
      minVerticalPadding: 6,
      horizontalTitleGap: 10,
      contentPadding: const EdgeInsets.symmetric(horizontal: 12),
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(HkTokens.radiusControl),
      ),
      titleTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 13,
        fontWeight: FontWeight.w500,
        color: t.text,
      ),
      subtitleTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 12,
        color: t.textMuted,
      ),
    ),
    expansionTileTheme: ExpansionTileThemeData(
      iconColor: t.textMuted,
      collapsedIconColor: t.textMuted,
      shape: const Border(),
      collapsedShape: const Border(),
      tilePadding: const EdgeInsets.symmetric(horizontal: 12),
    ),
    scrollbarTheme: ScrollbarThemeData(
      thickness: const WidgetStatePropertyAll(6),
      radius: const Radius.circular(3),
      thumbColor: WidgetStatePropertyAll(t.borderStrong),
      crossAxisMargin: 2,
    ),
    badgeTheme: BadgeThemeData(
      backgroundColor: t.accent2,
      textColor: t.onAccent,
    ),
    drawerTheme: DrawerThemeData(backgroundColor: t.surface),
    navigationRailTheme: NavigationRailThemeData(
      backgroundColor: t.surface,
      indicatorColor: t.accent.withValues(alpha: 0.18),
      selectedIconTheme: IconThemeData(color: t.accent),
      unselectedIconTheme: IconThemeData(color: t.textMuted),
      selectedLabelTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 11,
        color: t.text,
      ),
      unselectedLabelTextStyle: TextStyle(
        fontFamily: HkTokens.fontUi,
        fontSize: 11,
        color: t.textMuted,
      ),
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: t.surface,
      indicatorColor: t.accent.withValues(alpha: 0.2),
      height: 64,
      iconTheme: WidgetStateProperty.resolveWith(
        (s) => IconThemeData(
          color: s.contains(WidgetState.selected) ? t.accent : t.textMuted,
        ),
      ),
      labelTextStyle: WidgetStatePropertyAll(
        TextStyle(
          fontFamily: HkTokens.fontUi,
          fontSize: 11,
          fontWeight: FontWeight.w600,
          color: t.text,
        ),
      ),
    ),
    dividerTheme: DividerThemeData(color: t.border, space: 1, thickness: 1),
    textSelectionTheme: TextSelectionThemeData(
      cursorColor: t.accent,
      selectionColor: t.accent.withValues(alpha: 0.3),
      selectionHandleColor: t.accent,
    ),
  );
}
