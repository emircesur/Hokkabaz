import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_markdown_plus/flutter_markdown_plus.dart';
import 'package:markdown/markdown.dart' as md;

import '../../app/breakpoints.dart';
import '../../ffi/voice.dart';
import '../../theme/tokens.dart';
import '../../widgets/hk_ui.dart';
import 'chat_model.dart';

/// Max reading width for the message column (long lines are hard to read).
const double kMessageMaxWidth = 860;

class UserBubble extends StatefulWidget {
  const UserBubble({super.key, required this.item, this.onEdit});

  final UserItem item;

  /// Puts the text back into the composer.
  final VoidCallback? onEdit;

  @override
  State<UserBubble> createState() => _UserBubbleState();
}

class _UserBubbleState extends State<UserBubble> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    Widget act(IconData i, String tip, VoidCallback f) => IconButton(
      tooltip: tip,
      iconSize: 14,
      visualDensity: VisualDensity.compact,
      constraints: const BoxConstraints(minWidth: 26, minHeight: 26),
      icon: Icon(i, color: hk.textFaint),
      onPressed: f,
    );
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      child: Row(
        mainAxisAlignment: MainAxisAlignment.end,
        crossAxisAlignment: CrossAxisAlignment.end,
        children: [
          AnimatedOpacity(
            duration: const Duration(milliseconds: 120),
            opacity: _hover ? 1 : 0,
            child: Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                act(Icons.copy_rounded, 'Copy', () {
                  Clipboard.setData(ClipboardData(text: widget.item.text));
                }),
                if (widget.onEdit != null)
                  act(Icons.edit_outlined, 'Edit & resend', widget.onEdit!),
              ],
            ),
          ),
          Flexible(
            child: ConstrainedBox(
              constraints: const BoxConstraints(
                maxWidth: kMessageMaxWidth * 0.75,
              ),
              child: Container(
                padding: const EdgeInsets.symmetric(
                  horizontal: 14,
                  vertical: 10,
                ),
                decoration: BoxDecoration(
                  color: hk.surface3,
                  borderRadius: const BorderRadius.only(
                    topLeft: Radius.circular(16),
                    topRight: Radius.circular(16),
                    bottomLeft: Radius.circular(16),
                    bottomRight: Radius.circular(4),
                  ),
                ),
                child: SelectableText(
                  widget.item.text,
                  style: const TextStyle(fontSize: 14, height: 1.5),
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

class AssistantBlock extends StatelessWidget {
  const AssistantBlock({super.key, required this.item, this.onRegenerate});

  final AssistantItem item;

  /// Set on the latest answer only.
  final VoidCallback? onRegenerate;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Padding(
          padding: const EdgeInsets.only(top: 2, right: HkTokens.s3),
          child: Container(
            width: 26,
            height: 26,
            decoration: BoxDecoration(
              gradient: hk.gradient,
              borderRadius: BorderRadius.circular(8),
            ),
            child: Icon(Icons.auto_awesome, size: 15, color: hk.onAccent),
          ),
        ),
        Expanded(
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (item.reasoning.isNotEmpty)
                _Reasoning(item: item, startOpen: item.streaming),
              if (item.text.isNotEmpty) MarkdownText(item.text),
              if (item.streaming && item.text.isEmpty && item.reasoning.isEmpty)
                const _Typing(),
              if (item.text.isNotEmpty && !item.streaming)
                _Actions(text: item.text, onRegenerate: onRegenerate),
            ],
          ),
        ),
      ],
    );
  }
}

/// Copy / read aloud under a finished answer.
class _Actions extends StatelessWidget {
  const _Actions({required this.text, this.onRegenerate});

  final String text;
  final VoidCallback? onRegenerate;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    Widget btn(IconData i, String tip, VoidCallback f) => IconButton(
      tooltip: tip,
      visualDensity: VisualDensity.compact,
      iconSize: 15,
      constraints: const BoxConstraints(minWidth: 28, minHeight: 28),
      icon: Icon(i, color: hk.textFaint),
      onPressed: f,
    );
    return Padding(
      padding: const EdgeInsets.only(top: 2),
      child: Row(
        children: [
          btn(Icons.copy_rounded, 'Copy', () {
            Clipboard.setData(ClipboardData(text: text));
            ScaffoldMessenger.of(context)
                .showSnackBar(const SnackBar(content: Text('Copied')));
          }),
          btn(
            Icons.volume_up_outlined,
            'Read aloud',
            () => Voice.instance.speak(text).catchError((Object e) {
              if (context.mounted) {
                ScaffoldMessenger.of(context)
                    .showSnackBar(SnackBar(content: Text('Read aloud: $e')));
              }
            }),
          ),
          if (onRegenerate != null)
            btn(Icons.refresh_rounded, 'Regenerate', onRegenerate!),
        ],
      ),
    );
  }
}

class _Typing extends StatefulWidget {
  const _Typing();

  @override
  State<_Typing> createState() => _TypingState();
}

class _TypingState extends State<_Typing> with SingleTickerProviderStateMixin {
  late final _c = AnimationController(
    vsync: this,
    duration: const Duration(milliseconds: 1100),
  )..repeat();

  @override
  void dispose() {
    _c.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return SizedBox(
      height: 26,
      child: AnimatedBuilder(
        animation: _c,
        builder: (_, _) => Row(
          children: [
            for (var i = 0; i < 3; i++)
              Container(
                width: 6,
                height: 6,
                margin: const EdgeInsets.only(right: 4),
                decoration: BoxDecoration(
                  shape: BoxShape.circle,
                  color: hk.textMuted.withValues(
                    alpha: 0.3 + 0.7 * ((_c.value * 3 - i) % 3 < 1 ? 1 : 0),
                  ),
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// Markdown for model output: headings, lists, tables, quotes, code blocks with a copy button.
class MarkdownText extends StatelessWidget {
  const MarkdownText(this.data, {super.key, this.fontSize = 14});

  final String data;
  final double fontSize;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final base = TextStyle(fontSize: fontSize, height: 1.6, color: hk.text);
    final mono = TextStyle(
      fontFamily: HkTokens.fontMono,
      fontSize: fontSize - 1.5,
      color: hk.text,
    );
    return MarkdownBody(
      data: data,
      selectable: true,
      softLineBreak: true,
      extensionSet: md.ExtensionSet.gitHubWeb,
      builders: {'pre': _CodeBlockBuilder(hk)},
      styleSheet: MarkdownStyleSheet(
        p: base,
        a: base.copyWith(
          color: hk.accent,
          decoration: TextDecoration.underline,
        ),
        strong: base.copyWith(fontWeight: FontWeight.w700),
        em: base.copyWith(fontStyle: FontStyle.italic),
        h1: base.copyWith(fontSize: fontSize + 8, fontWeight: FontWeight.w700),
        h2: base.copyWith(fontSize: fontSize + 5, fontWeight: FontWeight.w700),
        h3: base.copyWith(fontSize: fontSize + 2, fontWeight: FontWeight.w600),
        h4: base.copyWith(fontWeight: FontWeight.w600),
        h1Padding: const EdgeInsets.only(top: 8),
        h2Padding: const EdgeInsets.only(top: 8),
        h3Padding: const EdgeInsets.only(top: 6),
        listBullet: base.copyWith(color: hk.textMuted),
        blockSpacing: 10,
        code: mono.copyWith(backgroundColor: hk.surface3),
        codeblockDecoration: const BoxDecoration(),
        codeblockPadding: EdgeInsets.zero,
        blockquote: base.copyWith(color: hk.textMuted),
        blockquoteDecoration: BoxDecoration(
          border: Border(left: BorderSide(color: hk.accent, width: 3)),
        ),
        blockquotePadding: const EdgeInsets.only(left: 12, top: 2, bottom: 2),
        tableHead: base.copyWith(fontWeight: FontWeight.w600),
        tableBody: base,
        tableBorder: TableBorder.all(color: hk.border),
        tableCellsPadding: const EdgeInsets.symmetric(
          horizontal: 10,
          vertical: 6,
        ),
        horizontalRuleDecoration: BoxDecoration(
          border: Border(top: BorderSide(color: hk.border)),
        ),
      ),
    );
  }
}

class _CodeBlockBuilder extends MarkdownElementBuilder {
  _CodeBlockBuilder(this.hk);

  final HkTokens hk;

  @override
  Widget? visitElementAfterWithContext(
    BuildContext context,
    md.Element element,
    TextStyle? preferredStyle,
    TextStyle? parentStyle,
  ) {
    final code = element.textContent.replaceFirst(RegExp(r'\n$'), '');
    var lang = '';
    final child = element.children?.firstOrNull;
    if (child is md.Element) {
      final cls = child.attributes['class'] ?? '';
      if (cls.startsWith('language-')) lang = cls.substring(9);
    }
    return _CodeBlock(code: code, lang: lang);
  }
}

class _CodeBlock extends StatelessWidget {
  const _CodeBlock({required this.code, required this.lang});

  final String code;
  final String lang;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.symmetric(vertical: 4),
      decoration: BoxDecoration(
        color: hk.bg,
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        border: Border.all(color: hk.border),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Container(
            height: 32,
            padding: const EdgeInsets.only(left: 12, right: 4),
            decoration: BoxDecoration(
              color: hk.surface2,
              border: Border(bottom: BorderSide(color: hk.border)),
              borderRadius: const BorderRadius.vertical(
                top: Radius.circular(HkTokens.radiusCard),
              ),
            ),
            child: Row(
              children: [
                Text(
                  lang.isEmpty ? 'code' : lang,
                  style: TextStyle(
                    fontSize: 11,
                    fontFamily: HkTokens.fontMono,
                    color: hk.textMuted,
                  ),
                ),
                const Spacer(),
                TextButton.icon(
                  style: TextButton.styleFrom(
                    foregroundColor: hk.textMuted,
                    visualDensity: VisualDensity.compact,
                  ),
                  onPressed: () {
                    Clipboard.setData(ClipboardData(text: code));
                    ScaffoldMessenger.of(context).showSnackBar(
                      const SnackBar(content: Text('Code copied')),
                    );
                  },
                  icon: const Icon(Icons.copy_rounded, size: 13),
                  label: const Text('Copy', style: TextStyle(fontSize: 11)),
                ),
              ],
            ),
          ),
          SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            padding: const EdgeInsets.all(12),
            child: SelectableText(
              code,
              style: TextStyle(
                fontFamily: HkTokens.fontMono,
                fontSize: 12.5,
                height: 1.55,
                color: hk.text,
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// Collapsed by default with a token count; opens to the raw reasoning text.
class _Reasoning extends StatefulWidget {
  const _Reasoning({required this.item, required this.startOpen});

  final AssistantItem item;
  final bool startOpen;

  @override
  State<_Reasoning> createState() => _ReasoningState();
}

class _ReasoningState extends State<_Reasoning> {
  late bool _open = widget.startOpen;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Padding(
      padding: const EdgeInsets.only(bottom: HkTokens.s2),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusChip),
            onTap: () => setState(() => _open = !_open),
            child: ConstrainedBox(
              constraints: BoxConstraints(minHeight: touchMinHeight(context)),
              child: Pill(
                'Thinking · ${widget.item.reasoningLabel}',
                icon: _open ? Icons.expand_less : Icons.psychology_outlined,
                color: hk.accent,
              ),
            ),
          ),
          if (_open)
            Container(
              width: double.infinity,
              margin: const EdgeInsets.only(top: HkTokens.s2),
              padding: const EdgeInsets.only(left: HkTokens.s3),
              decoration: BoxDecoration(
                border: Border(
                  left: BorderSide(color: hk.borderStrong, width: 2),
                ),
              ),
              child: SelectableText(
                widget.item.reasoning,
                style: TextStyle(
                  fontSize: 13,
                  height: 1.5,
                  color: hk.textMuted,
                ),
              ),
            ),
        ],
      ),
    );
  }
}

class NoticeLine extends StatelessWidget {
  const NoticeLine({super.key, required this.item});

  final NoticeItem item;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final err = item.kind == NoticeKind.error;
    final color = err ? hk.err : hk.textMuted;
    return Container(
      padding: const EdgeInsets.symmetric(
        horizontal: HkTokens.s3,
        vertical: HkTokens.s2,
      ),
      decoration: BoxDecoration(
        color: err ? hk.err.withValues(alpha: 0.08) : hk.surface2,
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        border: Border.all(
          color: err ? hk.err.withValues(alpha: 0.4) : hk.border,
        ),
      ),
      child: Row(
        children: [
          Icon(
            err ? Icons.error_outline : Icons.info_outline,
            size: 16,
            color: color,
          ),
          const SizedBox(width: HkTokens.s2),
          Expanded(
            child: SelectableText(
              item.text,
              style: TextStyle(fontSize: 13, color: color),
            ),
          ),
        ],
      ),
    );
  }
}
