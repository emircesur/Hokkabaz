import 'dart:convert';

import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'approval_card.dart';
import 'chat_model.dart';
import 'diff_view.dart';

/// Compact tool-call card: name, target, status; expands to args/output/diff.
class ToolCard extends StatefulWidget {
  const ToolCard({super.key, required this.sessionId, required this.item});

  final String sessionId;
  final ToolItem item;

  @override
  State<ToolCard> createState() => _ToolCardState();
}

class _ToolCardState extends State<ToolCard> {
  bool _open = false;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final t = widget.item;
    final diff = diffForTool(t.name, t.args);
    final waiting = t.status == ToolStatus.awaitingApproval;
    final approval = t.approval;
    // Approvals must be visible without a tap; everything else is on demand.
    final expanded = _open || waiting;
    return Container(
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: waiting ? hk.warn : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            onTap: () => setState(() => _open = !_open),
            child: Padding(
              padding: const EdgeInsets.symmetric(
                horizontal: HkTokens.s3,
                vertical: HkTokens.s2,
              ),
              child: Row(
                children: [
                  Icon(Icons.build_outlined, size: 14, color: hk.textMuted),
                  const SizedBox(width: HkTokens.s2),
                  Text(
                    t.name.isEmpty ? 'tool' : t.name,
                    style: const TextStyle(
                      fontWeight: FontWeight.w600,
                      fontSize: 13,
                    ),
                  ),
                  const SizedBox(width: HkTokens.s2),
                  Expanded(
                    child: Text(
                      t.target,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: TextStyle(
                        fontFamily: HkTokens.fontMono,
                        fontSize: 12,
                        color: hk.textMuted,
                      ),
                    ),
                  ),
                  if (diff != null && t.status == ToolStatus.done)
                    Padding(
                      padding: const EdgeInsets.only(right: HkTokens.s2),
                      child: Text(
                        'diff',
                        style: TextStyle(fontSize: 12, color: hk.info),
                      ),
                    ),
                  _StatusIcon(status: t.status),
                  Icon(
                    expanded ? Icons.expand_less : Icons.expand_more,
                    size: 18,
                    color: hk.textMuted,
                  ),
                ],
              ),
            ),
          ),
          if (waiting && approval != null && !approval.resolved)
            Padding(
              padding: const EdgeInsets.fromLTRB(
                HkTokens.s3,
                0,
                HkTokens.s3,
                HkTokens.s3,
              ),
              child: ApprovalCard(
                sessionId: widget.sessionId,
                tool: t,
                approval: approval,
              ),
            ),
          if (_open) ...[
            Divider(height: 1, color: hk.border),
            Padding(
              padding: const EdgeInsets.all(HkTokens.s3),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  if (diff != null) ...[
                    DiffView(lines: diff),
                  ] else ...[
                    const _Label('Arguments'),
                    _Mono(_prettyJson(t.argsJson)),
                  ],
                  if (approval != null && approval.resolved) ...[
                    const SizedBox(height: HkTokens.s2),
                    Text(
                      approval.approved
                          ? 'Approved'
                          : 'Denied${(approval.reason ?? '').isEmpty ? '' : ': ${approval.reason}'}',
                      style: TextStyle(
                        fontSize: 12,
                        color: approval.approved ? hk.ok : hk.err,
                      ),
                    ),
                  ],
                  if (t.result != null) ...[
                    const SizedBox(height: HkTokens.s3),
                    _Label(t.status == ToolStatus.error ? 'Error' : 'Output'),
                    _Mono(
                      t.result!,
                      color: t.status == ToolStatus.error ? hk.err : null,
                      maxChars: 8000,
                    ),
                  ],
                ],
              ),
            ),
          ],
        ],
      ),
    );
  }
}

String _prettyJson(String raw) {
  try {
    return const JsonEncoder.withIndent('  ').convert(jsonDecode(raw));
  } catch (_) {
    return raw;
  }
}

class _StatusIcon extends StatelessWidget {
  const _StatusIcon({required this.status});

  final ToolStatus status;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    switch (status) {
      case ToolStatus.streaming:
      case ToolStatus.running:
        return SizedBox(
          width: 14,
          height: 14,
          child: CircularProgressIndicator(strokeWidth: 2, color: hk.accent),
        );
      case ToolStatus.awaitingApproval:
        return Icon(Icons.hourglass_top, size: 16, color: hk.warn);
      case ToolStatus.done:
        return Icon(Icons.check_circle, size: 16, color: hk.ok);
      case ToolStatus.error:
        return Icon(Icons.error, size: 16, color: hk.err);
      case ToolStatus.denied:
        return Icon(Icons.block, size: 16, color: hk.err);
    }
  }
}

class _Label extends StatelessWidget {
  const _Label(this.text);

  final String text;

  @override
  Widget build(BuildContext context) => Padding(
    padding: const EdgeInsets.only(bottom: HkTokens.s1),
    child: Text(
      text,
      style: TextStyle(fontSize: 12, color: context.hk.textMuted),
    ),
  );
}

class _Mono extends StatelessWidget {
  const _Mono(this.text, {this.color, this.maxChars});

  final String text;
  final Color? color;
  final int? maxChars;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final cut = maxChars != null && text.length > maxChars!;
    return Container(
      width: double.infinity,
      padding: const EdgeInsets.all(HkTokens.s2),
      decoration: BoxDecoration(
        color: hk.bg,
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: SelectableText(
        cut
            ? '${text.substring(0, maxChars)}\n… (${text.length - maxChars!} more chars)'
            : text,
        style: TextStyle(
          fontFamily: HkTokens.fontMono,
          fontSize: 12,
          height: 1.35,
          color: color ?? hk.text,
        ),
      ),
    );
  }
}
