import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../ffi/notifications.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';

/// Every tool call waiting on you, across sessions, answerable in one place.
Future<void> showApprovalsInbox(BuildContext context) => showModalBottomSheet<void>(
  context: context,
  isScrollControlled: true,
  showDragHandle: true,
  builder: (_) => const FractionallySizedBox(heightFactor: 0.75, child: ApprovalsInbox()),
);

class ApprovalsInbox extends ConsumerWidget {
  const ApprovalsInbox({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final pending = ref.watch(approvalInboxProvider.select((m) => [for (final l in m.values) ...l]));
    final hk = context.hk;
    if (pending.isEmpty) {
      return const Center(child: Text('Nothing is waiting for approval.'));
    }
    return ListView.separated(
      padding: const EdgeInsets.all(HkTokens.s3),
      itemCount: pending.length,
      separatorBuilder: (_, _) => const SizedBox(height: HkTokens.s2),
      itemBuilder: (context, i) {
        final n = pending[i];
        final ctl = ref.read(chatControllerProvider(n.sessionId).notifier);
        final session = ref.watch(sessionListProvider).value?.where((s) => s.id == n.sessionId).firstOrNull;
        return Container(
          padding: const EdgeInsets.all(HkTokens.s3),
          decoration: BoxDecoration(
            color: hk.warn.withValues(alpha: 0.10),
            border: Border.all(color: hk.warn.withValues(alpha: 0.5)),
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(n.tool, style: const TextStyle(fontWeight: FontWeight.w700)),
              Text(session?.title ?? n.sessionId, style: TextStyle(color: hk.textMuted, fontSize: 12)),
              if (n.detail != null && n.detail!.isNotEmpty) ...[
                const SizedBox(height: HkTokens.s2),
                SelectableText(n.detail!, style: const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 12)),
              ],
              const SizedBox(height: HkTokens.s2),
              Wrap(
                spacing: HkTokens.s2,
                runSpacing: HkTokens.s1,
                alignment: WrapAlignment.end,
                children: [
                  TextButton(
                    style: TextButton.styleFrom(minimumSize: const Size(88, kTouchTarget)),
                    onPressed: () => ctl.allowAlways(n.tool),
                    child: Text('Always allow ${n.tool}'),
                  ),
                  OutlinedButton(
                    style: OutlinedButton.styleFrom(minimumSize: const Size(88, kTouchTarget)),
                    onPressed: () => ctl.respond(n.approvalId, approved: false),
                    child: const Text('Deny'),
                  ),
                  FilledButton(
                    style: FilledButton.styleFrom(minimumSize: const Size(88, kTouchTarget)),
                    onPressed: () => ctl.respond(n.approvalId, approved: true),
                    child: const Text('Approve'),
                  ),
                ],
              ),
            ],
          ),
        );
      },
    );
  }
}
