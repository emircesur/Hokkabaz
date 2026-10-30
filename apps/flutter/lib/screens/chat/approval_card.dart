import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../theme/tokens.dart';
import 'chat_model.dart';
import 'chat_providers.dart';

/// Inline approve/deny prompt for a tool call waiting on the user.
class ApprovalCard extends ConsumerStatefulWidget {
  const ApprovalCard({
    super.key,
    required this.sessionId,
    required this.tool,
    required this.approval,
  });

  final String sessionId;
  final ToolItem tool;
  final ApprovalState approval;

  @override
  ConsumerState<ApprovalCard> createState() => _ApprovalCardState();
}

class _ApprovalCardState extends ConsumerState<ApprovalCard> {
  final _reason = TextEditingController();
  bool _busy = false;

  @override
  void dispose() {
    _reason.dispose();
    super.dispose();
  }

  Future<void> _always() async {
    setState(() => _busy = true);
    ref
        .read(chatControllerProvider(widget.sessionId).notifier)
        .allowAlways(widget.tool.name);
    if (mounted) setState(() => _busy = false);
  }

  Future<void> _respond(bool approved) async {
    setState(() => _busy = true);
    await ref
        .read(chatControllerProvider(widget.sessionId).notifier)
        .respond(widget.approval.id, approved: approved, reason: _reason.text);
    // On success the resolved event rebuilds this card away; on failure let
    // the user retry.
    if (mounted) setState(() => _busy = false);
  }

  /// Full-width 48dp buttons on touch layouts; compact pair on desktop.
  Widget _buttons(BuildContext context) {
    final narrow = isNarrow(context);
    final style = narrow
        ? ButtonStyle(
            minimumSize: const WidgetStatePropertyAll(Size(88, kTouchTarget)),
          )
        : null;
    final deny = OutlinedButton(
      style: style,
      onPressed: _busy ? null : () => _respond(false),
      child: const Text('Deny'),
    );
    final approve = FilledButton(
      style: style,
      onPressed: _busy ? null : () => _respond(true),
      child: const Text('Approve'),
    );
    final always = TextButton(
      style: style,
      onPressed: _busy ? null : _always,
      child: Text(
        'Always allow ${widget.tool.name}',
        overflow: TextOverflow.ellipsis,
      ),
    );
    if (narrow) {
      return Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(child: deny),
              const SizedBox(width: HkTokens.s2),
              Expanded(child: approve),
            ],
          ),
          always,
        ],
      );
    }
    return Row(
      mainAxisAlignment: MainAxisAlignment.end,
      children: [
        Flexible(child: always),
        const SizedBox(width: HkTokens.s2),
        deny,
        const SizedBox(width: HkTokens.s2),
        approve,
      ],
    );
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final detail = widget.approval.detail;
    return Container(
      margin: const EdgeInsets.only(top: HkTokens.s2),
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.warn.withValues(alpha: 0.10),
        border: Border.all(color: hk.warn.withValues(alpha: 0.5)),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Icon(Icons.shield_outlined, size: 16, color: hk.warn),
              const SizedBox(width: HkTokens.s2),
              Expanded(
                child: Text(
                  'Approval needed: ${widget.tool.name}',
                  style: const TextStyle(fontWeight: FontWeight.w600),
                ),
              ),
            ],
          ),
          if (detail != null && detail.isNotEmpty) ...[
            const SizedBox(height: HkTokens.s2),
            SelectableText(
              detail,
              style: const TextStyle(
                fontFamily: HkTokens.fontMono,
                fontSize: 12,
              ),
            ),
          ],
          const SizedBox(height: HkTokens.s2),
          TextField(
            controller: _reason,
            enabled: !_busy,
            decoration: InputDecoration(
              isDense: !isNarrow(context),
              hintText: 'Reason (optional)',
              border: const OutlineInputBorder(),
            ),
          ),
          const SizedBox(height: HkTokens.s2),
          _buttons(context),
        ],
      ),
    );
  }
}
