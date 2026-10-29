import 'package:flutter/material.dart';

/// Design tokens (DESIGN.md §4.7). Pro-editor look: layered graphite panels, one violet accent
/// with a violet→pink gradient for primary actions. Widgets read these, never raw hex.
@immutable
class HkTokens extends ThemeExtension<HkTokens> {
  const HkTokens({
    required this.bg,
    required this.surface,
    required this.surface2,
    required this.surface3,
    required this.border,
    required this.borderStrong,
    required this.text,
    required this.textMuted,
    required this.textFaint,
    required this.accent,
    required this.accent2,
    required this.onAccent,
    required this.ok,
    required this.warn,
    required this.err,
    required this.info,
  });

  /// App background (behind panels).
  final Color bg;

  /// Panels, rails, sidebars.
  final Color surface;

  /// Raised: cards, inputs, menus.
  final Color surface2;

  /// Hover / selected rows.
  final Color surface3;
  final Color border;
  final Color borderStrong;
  final Color text;
  final Color textMuted;
  final Color textFaint;
  final Color accent;
  final Color accent2;
  final Color onAccent;
  final Color ok;
  final Color warn;
  final Color err;
  final Color info;

  LinearGradient get gradient => LinearGradient(
    colors: [accent, accent2],
    begin: Alignment.topLeft,
    end: Alignment.bottomRight,
  );

  // Node category stripes.
  static const Color nodeTrigger = Color(0xFFF2A33A);
  static const Color nodeAi = Color(0xFF8B7CF6);
  static const Color nodeLogic = Color(0xFF64748B);
  static const Color nodeData = Color(0xFF2BB3A3);
  static const Color nodeIntegration = Color(0xFF4C9AFF);

  // Radii.
  static const double radiusControl = 8;
  static const double radiusCard = 10;
  static const double radiusSheet = 14;
  static const double radiusChip = 999;

  // 4-pt grid.
  static const double s1 = 4;
  static const double s2 = 8;
  static const double s3 = 12;
  static const double s4 = 16;
  static const double s5 = 20;
  static const double s6 = 24;
  static const double s8 = 32;

  // Type scale.
  static const List<double> typeScale = [11, 12, 13, 14, 16, 20, 24, 30];
  static const String fontUi = 'Inter';
  static const String fontMono = 'JetBrains Mono';

  static const HkTokens dark = HkTokens(
    bg: Color(0xFF09090C),
    surface: Color(0xFF111116),
    surface2: Color(0xFF18181F),
    surface3: Color(0xFF22222B),
    border: Color(0xFF24242D),
    borderStrong: Color(0xFF34343F),
    text: Color(0xFFF1F1F4),
    textMuted: Color(0xFF9696A3),
    textFaint: Color(0xFF62626E),
    accent: Color(0xFF8B5CF6),
    accent2: Color(0xFFEC4899),
    onAccent: Color(0xFFFFFFFF),
    ok: Color(0xFF22C55E),
    warn: Color(0xFFF59E0B),
    err: Color(0xFFEF4444),
    info: Color(0xFF3B82F6),
  );

  static const HkTokens light = HkTokens(
    bg: Color(0xFFF3F3F6),
    surface: Color(0xFFFFFFFF),
    surface2: Color(0xFFF6F6F9),
    surface3: Color(0xFFECECF1),
    border: Color(0xFFE4E4EA),
    borderStrong: Color(0xFFD1D1DA),
    text: Color(0xFF111116),
    textMuted: Color(0xFF5E5E6C),
    textFaint: Color(0xFF9A9AA6),
    accent: Color(0xFF7C3AED),
    accent2: Color(0xFFDB2777),
    onAccent: Color(0xFFFFFFFF),
    ok: Color(0xFF16A34A),
    warn: Color(0xFFD97706),
    err: Color(0xFFDC2626),
    info: Color(0xFF2563EB),
  );

  @override
  HkTokens copyWith({
    Color? bg,
    Color? surface,
    Color? surface2,
    Color? surface3,
    Color? border,
    Color? borderStrong,
    Color? text,
    Color? textMuted,
    Color? textFaint,
    Color? accent,
    Color? accent2,
    Color? onAccent,
    Color? ok,
    Color? warn,
    Color? err,
    Color? info,
  }) => HkTokens(
    bg: bg ?? this.bg,
    surface: surface ?? this.surface,
    surface2: surface2 ?? this.surface2,
    surface3: surface3 ?? this.surface3,
    border: border ?? this.border,
    borderStrong: borderStrong ?? this.borderStrong,
    text: text ?? this.text,
    textMuted: textMuted ?? this.textMuted,
    textFaint: textFaint ?? this.textFaint,
    accent: accent ?? this.accent,
    accent2: accent2 ?? this.accent2,
    onAccent: onAccent ?? this.onAccent,
    ok: ok ?? this.ok,
    warn: warn ?? this.warn,
    err: err ?? this.err,
    info: info ?? this.info,
  );

  @override
  HkTokens lerp(ThemeExtension<HkTokens>? other, double t) {
    if (other is! HkTokens) return this;
    Color l(Color a, Color b) => Color.lerp(a, b, t)!;
    return HkTokens(
      bg: l(bg, other.bg),
      surface: l(surface, other.surface),
      surface2: l(surface2, other.surface2),
      surface3: l(surface3, other.surface3),
      border: l(border, other.border),
      borderStrong: l(borderStrong, other.borderStrong),
      text: l(text, other.text),
      textMuted: l(textMuted, other.textMuted),
      textFaint: l(textFaint, other.textFaint),
      accent: l(accent, other.accent),
      accent2: l(accent2, other.accent2),
      onAccent: l(onAccent, other.onAccent),
      ok: l(ok, other.ok),
      warn: l(warn, other.warn),
      err: l(err, other.err),
      info: l(info, other.info),
    );
  }
}

extension HkTokensContext on BuildContext {
  HkTokens get hk => Theme.of(this).extension<HkTokens>()!;
}
