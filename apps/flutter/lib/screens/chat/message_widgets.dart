import 'package:flutter/material.dart';

import '../../app/breakpoints.dart';
import '../../theme/tokens.dart';
import 'chat_model.dart';

class UserBubble extends StatelessWidget {
  const UserBubble({super.key, required this.item});

  final UserItem item;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Align(
      alignment: Alignment.centerRight,
      child: ConstrainedBox(
        constraints: BoxConstraints(
          maxWidth: MediaQuery.sizeOf(context).width * 0.8,
        ),
        child: Container(
          padding: const EdgeInsets.symmetric(
            horizontal: HkTokens.s3,
            vertical: HkTokens.s2,
          ),
          decoration: BoxDecoration(
            color: hk.surface,
            border: Border.all(color: hk.border),
            borderRadius: BorderRadius.circular(HkTokens.radiusSheet),
          ),
          child: SelectableText(item.text),
        ),
      ),
    );
  }
}

class AssistantBlock extends StatelessWidget {
  const AssistantBlock({super.key, required this.item});

  final AssistantItem item;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (item.reasoning.isNotEmpty)
          _Reasoning(item: item, startOpen: item.streaming),
        if (item.text.isNotEmpty) SelectableText(item.text),
        if (item.streaming && item.text.isEmpty && item.reasoning.isEmpty)
          Text('…', style: TextStyle(color: hk.textMuted)),
      ],
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
            onTap: () => setState(() => _open = !_open),
            child: ConstrainedBox(
              constraints: BoxConstraints(minHeight: touchMinHeight(context)),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(
                    _open ? Icons.arrow_drop_down : Icons.arrow_right,
                    size: 18,
                    color: hk.accent2,
                  ),
                  Text(
                    'Reasoning · ${widget.item.reasoningLabel}',
                    style: TextStyle(fontSize: 12, color: hk.accent2),
                  ),
                ],
              ),
            ),
          ),
          if (_open)
            Container(
              width: double.infinity,
              margin: const EdgeInsets.only(left: HkTokens.s2),
              padding: const EdgeInsets.only(left: HkTokens.s3),
              decoration: BoxDecoration(
                border: Border(left: BorderSide(color: hk.border, width: 2)),
              ),
              child: SelectableText(
                widget.item.reasoning,
                style: TextStyle(
                  fontSize: 13,
                  color: hk.textMuted,
                  fontStyle: FontStyle.italic,
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
        color: err ? hk.err.withValues(alpha: 0.10) : null,
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        border: err ? Border.all(color: hk.err.withValues(alpha: 0.5)) : null,
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
