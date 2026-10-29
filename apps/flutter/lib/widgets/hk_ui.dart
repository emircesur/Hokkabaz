import 'package:flutter/material.dart';

import '../theme/tokens.dart';

/// Shared building blocks for the pro-editor look: panels with headers, gradient primary
/// buttons, labelled slider rows, pills, empty states.

/// Docked panel: header bar (title + actions) over a body, like an editor's tool panel.
class HkPanel extends StatelessWidget {
  const HkPanel({
    super.key,
    this.title,
    this.icon,
    this.actions = const [],
    required this.child,
    this.padding = EdgeInsets.zero,
    this.color,
  });

  final String? title;
  final IconData? icon;
  final List<Widget> actions;
  final Widget child;
  final EdgeInsets padding;
  final Color? color;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return ColoredBox(
      color: color ?? hk.surface,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          if (title != null) PanelHeader(title: title!, icon: icon, actions: actions),
          Expanded(
            child: Padding(padding: padding, child: child),
          ),
        ],
      ),
    );
  }
}

class PanelHeader extends StatelessWidget {
  const PanelHeader({
    super.key,
    required this.title,
    this.icon,
    this.actions = const [],
    this.height = 44,
  });

  final String title;
  final IconData? icon;
  final List<Widget> actions;
  final double height;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      height: height,
      padding: const EdgeInsets.only(left: HkTokens.s4, right: HkTokens.s2),
      decoration: BoxDecoration(
        border: Border(bottom: BorderSide(color: hk.border)),
      ),
      child: Row(
        children: [
          if (icon != null) ...[
            Icon(icon, size: 16, color: hk.textMuted),
            const SizedBox(width: HkTokens.s2),
          ],
          Expanded(
            child: Text(
              title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontSize: 13, fontWeight: FontWeight.w600),
            ),
          ),
          ...actions,
        ],
      ),
    );
  }
}

/// Small uppercase group label inside panels.
class SectionLabel extends StatelessWidget {
  const SectionLabel(this.text, {super.key, this.trailing, this.padding});

  final String text;
  final Widget? trailing;
  final EdgeInsets? padding;

  @override
  Widget build(BuildContext context) => Padding(
    padding: padding ?? const EdgeInsets.fromLTRB(0, HkTokens.s4, 0, HkTokens.s2),
    child: Row(
      children: [
        Expanded(
          child: Text(
            text.toUpperCase(),
            style: Theme.of(context).textTheme.labelSmall,
          ),
        ),
        ?trailing,
      ],
    ),
  );
}

/// Primary call to action with the accent gradient.
class GradientButton extends StatelessWidget {
  const GradientButton({
    super.key,
    required this.label,
    this.icon,
    required this.onPressed,
    this.busy = false,
    this.height = 38,
    this.expand = false,
  });

  final String label;
  final IconData? icon;
  final VoidCallback? onPressed;
  final bool busy;
  final double height;
  final bool expand;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final enabled = onPressed != null && !busy;
    final body = Row(
      mainAxisSize: expand ? MainAxisSize.max : MainAxisSize.min,
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        if (busy)
          SizedBox(
            width: 15,
            height: 15,
            child: CircularProgressIndicator(strokeWidth: 2, color: hk.onAccent),
          )
        else if (icon != null)
          Icon(icon, size: 17, color: hk.onAccent),
        if (busy || icon != null) const SizedBox(width: HkTokens.s2),
        Text(
          label,
          style: TextStyle(
            fontSize: 13,
            fontWeight: FontWeight.w600,
            color: hk.onAccent,
          ),
        ),
      ],
    );
    return Opacity(
      opacity: enabled ? 1 : 0.45,
      child: DecoratedBox(
        decoration: BoxDecoration(
          gradient: hk.gradient,
          borderRadius: BorderRadius.circular(HkTokens.radiusControl),
          boxShadow: enabled
              ? [
                  BoxShadow(
                    color: hk.accent.withValues(alpha: 0.35),
                    blurRadius: 16,
                    offset: const Offset(0, 4),
                  ),
                ]
              : null,
        ),
        child: Material(
          type: MaterialType.transparency,
          child: InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusControl),
            onTap: enabled ? onPressed : null,
            child: SizedBox(
              height: height,
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: HkTokens.s4),
                child: body,
              ),
            ),
          ),
        ),
      ),
    );
  }
}

/// Rounded tinted label (status, provider, tag).
class Pill extends StatelessWidget {
  const Pill(this.text, {super.key, this.color, this.icon, this.onTap});

  final String text;
  final Color? color;
  final IconData? icon;
  final VoidCallback? onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final c = color ?? hk.textMuted;
    final pill = Container(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 3),
      decoration: BoxDecoration(
        color: c.withValues(alpha: 0.12),
        borderRadius: BorderRadius.circular(HkTokens.radiusChip),
        border: Border.all(color: c.withValues(alpha: 0.25)),
      ),
      child: Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (icon != null) ...[
            Icon(icon, size: 12, color: c),
            const SizedBox(width: 4),
          ],
          Text(
            text,
            style: TextStyle(fontSize: 11, fontWeight: FontWeight.w600, color: c),
          ),
        ],
      ),
    );
    if (onTap == null) return pill;
    return InkWell(
      borderRadius: BorderRadius.circular(HkTokens.radiusChip),
      onTap: onTap,
      child: pill,
    );
  }
}

/// Label + value readout + slider; `null` value shows "Default" and an empty track.
class SliderRow extends StatelessWidget {
  const SliderRow({
    super.key,
    required this.label,
    required this.value,
    required this.min,
    required this.max,
    required this.onChanged,
    this.divisions,
    this.format,
    this.help,
    this.fallback,
  });

  final String label;
  final double? value;
  final double min;
  final double max;
  final int? divisions;
  final ValueChanged<double?> onChanged;
  final String Function(double)? format;
  final String? help;

  /// Shown when [value] is null, and where the thumb sits.
  final double? fallback;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final v = (value ?? fallback ?? min).clamp(min, max);
    final shown = value == null
        ? 'Default'
        : (format?.call(value!) ?? _fmt(value!));
    return Padding(
      padding: const EdgeInsets.only(bottom: HkTokens.s1),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Tooltip(
                  message: help ?? '',
                  child: Text(
                    label,
                    style: TextStyle(fontSize: 12, color: hk.textMuted),
                  ),
                ),
              ),
              Text(
                shown,
                style: TextStyle(
                  fontSize: 12,
                  fontFamily: HkTokens.fontMono,
                  color: value == null ? hk.textFaint : hk.text,
                ),
              ),
              if (value != null)
                InkWell(
                  onTap: () => onChanged(null),
                  child: Padding(
                    padding: const EdgeInsets.only(left: 4),
                    child: Icon(Icons.close, size: 13, color: hk.textFaint),
                  ),
                ),
            ],
          ),
          SizedBox(
            height: 26,
            child: SliderTheme(
              data: SliderTheme.of(context).copyWith(
                overlayShape: const RoundSliderOverlayShape(overlayRadius: 12),
                activeTrackColor: value == null ? hk.borderStrong : null,
              ),
              child: Slider(
                value: v,
                min: min,
                max: max,
                divisions: divisions,
                onChanged: onChanged,
              ),
            ),
          ),
        ],
      ),
    );
  }

  static String _fmt(double v) =>
      v == v.roundToDouble() ? v.toInt().toString() : v.toStringAsFixed(2);
}

/// Label + trailing control on one line (switches, dropdowns).
class SettingRow extends StatelessWidget {
  const SettingRow({
    super.key,
    required this.label,
    required this.child,
    this.help,
  });

  final String label;
  final Widget child;
  final String? help;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 3),
      child: Row(
        children: [
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(label, style: TextStyle(fontSize: 12, color: hk.text)),
                if (help != null)
                  Text(help!, style: TextStyle(fontSize: 11, color: hk.textFaint)),
              ],
            ),
          ),
          child,
        ],
      ),
    );
  }
}

/// Centered icon + title + hint + optional action.
class EmptyState extends StatelessWidget {
  const EmptyState({
    super.key,
    required this.icon,
    required this.title,
    this.message,
    this.action,
  });

  final IconData icon;
  final String title;
  final String? message;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(HkTokens.s6),
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Container(
                width: 56,
                height: 56,
                decoration: BoxDecoration(
                  gradient: LinearGradient(
                    colors: [
                      hk.accent.withValues(alpha: 0.25),
                      hk.accent2.withValues(alpha: 0.18),
                    ],
                  ),
                  borderRadius: BorderRadius.circular(16),
                  border: Border.all(color: hk.accent.withValues(alpha: 0.3)),
                ),
                child: Icon(icon, size: 26, color: hk.text),
              ),
              const SizedBox(height: HkTokens.s4),
              Text(
                title,
                textAlign: TextAlign.center,
                style: Theme.of(context).textTheme.titleLarge,
              ),
              if (message != null) ...[
                const SizedBox(height: HkTokens.s2),
                Text(
                  message!,
                  textAlign: TextAlign.center,
                  style: TextStyle(color: hk.textMuted, fontSize: 13),
                ),
              ],
              if (action != null) ...[
                const SizedBox(height: HkTokens.s5),
                action!,
              ],
            ],
          ),
        ),
      ),
    );
  }
}

/// Page header used at the top of full-width screens: title, subtitle, actions.
class PageHeader extends StatelessWidget {
  const PageHeader({
    super.key,
    required this.title,
    this.subtitle,
    this.actions = const [],
    this.bottom,
  });

  final String title;
  final String? subtitle;
  final List<Widget> actions;
  final Widget? bottom;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      padding: const EdgeInsets.fromLTRB(HkTokens.s6, HkTokens.s5, HkTokens.s6, 0),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(bottom: BorderSide(color: hk.border)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Text(title, style: Theme.of(context).textTheme.headlineSmall),
                    if (subtitle != null)
                      Padding(
                        padding: const EdgeInsets.only(top: 2),
                        child: Text(
                          subtitle!,
                          style: TextStyle(fontSize: 13, color: hk.textMuted),
                        ),
                      ),
                  ],
                ),
              ),
              for (final a in actions)
                Padding(padding: const EdgeInsets.only(left: HkTokens.s2), child: a),
            ],
          ),
          SizedBox(height: bottom == null ? HkTokens.s5 : HkTokens.s3),
          ?bottom,
        ],
      ),
    );
  }
}

/// Hoverable surface card with optional selection outline.
class HoverCard extends StatefulWidget {
  const HoverCard({
    super.key,
    required this.child,
    this.onTap,
    this.selected = false,
    this.padding = const EdgeInsets.all(HkTokens.s4),
  });

  final Widget child;
  final VoidCallback? onTap;
  final bool selected;
  final EdgeInsets padding;

  @override
  State<HoverCard> createState() => _HoverCardState();
}

class _HoverCardState extends State<HoverCard> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      cursor: widget.onTap != null ? SystemMouseCursors.click : MouseCursor.defer,
      child: GestureDetector(
        onTap: widget.onTap,
        child: AnimatedContainer(
          duration: const Duration(milliseconds: 140),
          padding: widget.padding,
          decoration: BoxDecoration(
            color: _hover ? hk.surface3 : hk.surface2,
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            border: Border.all(
              color: widget.selected
                  ? hk.accent
                  : (_hover ? hk.borderStrong : hk.border),
              width: widget.selected ? 1.5 : 1,
            ),
          ),
          child: widget.child,
        ),
      ),
    );
  }
}

/// Brand mark: gradient rounded square with an "H".
class BrandMark extends StatelessWidget {
  const BrandMark({super.key, this.size = 28});

  final double size;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      width: size,
      height: size,
      alignment: Alignment.center,
      decoration: BoxDecoration(
        gradient: hk.gradient,
        borderRadius: BorderRadius.circular(size * 0.28),
      ),
      child: Text(
        'H',
        style: TextStyle(
          color: hk.onAccent,
          fontWeight: FontWeight.w800,
          fontSize: size * 0.52,
          height: 1,
        ),
      ),
    );
  }
}
