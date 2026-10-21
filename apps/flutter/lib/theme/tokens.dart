import 'package:flutter/material.dart';

/// Design tokens from DESIGN.md §4.7. Widgets read these, never raw hex.
@immutable
class HkTokens extends ThemeExtension<HkTokens> {
  const HkTokens({
    required this.bg,
    required this.surface,
    required this.border,
    required this.text,
    required this.textMuted,
    required this.accent,
    required this.accent2,
    required this.ok,
    required this.warn,
    required this.err,
    required this.info,
  });

  final Color bg;
  final Color surface;
  final Color border;
  final Color text;
  final Color textMuted;
  final Color accent;
  final Color accent2;
  final Color ok;
  final Color warn;
  final Color err;
  final Color info;

  // Node category stripes.
  static const Color nodeTrigger = Color(0xFFF2A33A);
  static const Color nodeAi = Color(0xFF8B7CF6);
  static const Color nodeLogic = Color(0xFF64748B);
  static const Color nodeData = Color(0xFF2BB3A3);
  static const Color nodeIntegration = Color(0xFF4C9AFF);

  // Radii.
  static const double radiusCard = 6;
  static const double radiusSheet = 10;
  static const double radiusChip = 999;

  // 4-pt grid.
  static const double s1 = 4;
  static const double s2 = 8;
  static const double s3 = 12;
  static const double s4 = 16;
  static const double s6 = 24;

  // Type scale.
  static const List<double> typeScale = [12, 13, 14, 16, 20, 24];
  static const String fontUi = 'Inter';
  static const String fontMono = 'JetBrains Mono';

  static const HkTokens dark = HkTokens(
    bg: Color(0xFF0E0F13),
    surface: Color(0xFF16181E),
    border: Color(0xFF262A33),
    text: Color(0xFFE8E8EA),
    textMuted: Color(0xFF9A9CA5),
    accent: Color(0xFFF2A33A),
    accent2: Color(0xFF8B7CF6),
    ok: Color(0xFF3FB27F),
    warn: Color(0xFFE3B341),
    err: Color(0xFFE5534B),
    info: Color(0xFF4C9AFF),
  );

  static const HkTokens light = HkTokens(
    bg: Color(0xFFFAFAF7),
    surface: Color(0xFFFFFFFF),
    border: Color(0xFFE4E4DF),
    text: Color(0xFF17181C),
    textMuted: Color(0xFF5F616B),
    accent: Color(0xFFF2A33A),
    accent2: Color(0xFF8B7CF6),
    ok: Color(0xFF3FB27F),
    warn: Color(0xFFE3B341),
    err: Color(0xFFE5534B),
    info: Color(0xFF4C9AFF),
  );

  @override
  HkTokens copyWith({
    Color? bg,
    Color? surface,
    Color? border,
    Color? text,
    Color? textMuted,
    Color? accent,
    Color? accent2,
    Color? ok,
    Color? warn,
    Color? err,
    Color? info,
  }) => HkTokens(
    bg: bg ?? this.bg,
    surface: surface ?? this.surface,
    border: border ?? this.border,
    text: text ?? this.text,
    textMuted: textMuted ?? this.textMuted,
    accent: accent ?? this.accent,
    accent2: accent2 ?? this.accent2,
    ok: ok ?? this.ok,
    warn: warn ?? this.warn,
    err: err ?? this.err,
    info: info ?? this.info,
  );

  @override
  HkTokens lerp(ThemeExtension<HkTokens>? other, double t) {
    if (other is! HkTokens) return this;
    return HkTokens(
      bg: Color.lerp(bg, other.bg, t)!,
      surface: Color.lerp(surface, other.surface, t)!,
      border: Color.lerp(border, other.border, t)!,
      text: Color.lerp(text, other.text, t)!,
      textMuted: Color.lerp(textMuted, other.textMuted, t)!,
      accent: Color.lerp(accent, other.accent, t)!,
      accent2: Color.lerp(accent2, other.accent2, t)!,
      ok: Color.lerp(ok, other.ok, t)!,
      warn: Color.lerp(warn, other.warn, t)!,
      err: Color.lerp(err, other.err, t)!,
      info: Color.lerp(info, other.info, t)!,
    );
  }
}

extension HkTokensContext on BuildContext {
  HkTokens get hk => Theme.of(this).extension<HkTokens>()!;
}
