import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import '../flows/items_view.dart';
import '../flows/model/run_overlay.dart';
import '../flows/run_bridge.dart' show overlayOf;
import 'run_format.dart';
import 'runs_providers.dart';
import 'runs_screen.dart' show statusStyle;

/// One run: summary, error, per-node results, and "open with overlay".
class RunDetailScreen extends ConsumerWidget {
  const RunDetailScreen({super.key, required this.runId});

  final String runId;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final detail = ref.watch(runDetailProvider(runId));
    return Scaffold(
      appBar: AppBar(
        title: Text(
          detail.value?.summary.workflowName ?? 'Run',
          overflow: TextOverflow.ellipsis,
        ),
      ),
      body: detail.when(
        loading: () => const Center(child: CircularProgressIndicator()),
        error: (e, _) => Center(
          child: Padding(
            padding: const EdgeInsets.all(HkTokens.s4),
            child: Text(describeError(e), textAlign: TextAlign.center),
          ),
        ),
        data: (d) => _Body(detail: d),
      ),
    );
  }
}

class _Body extends ConsumerWidget {
  const _Body({required this.detail});

  final FlowRunDetailDto detail;

  void _open(BuildContext context, WidgetRef ref) {
    // Grabbed first: popping disposes this screen's context.
    final router = GoRouter.of(context);
    ref
        .read(pendingFlowOpenProvider.notifier)
        .set(
          PendingFlowOpen(
            flowId: localFlowIdOf(detail.workflowJson),
            workflowJson: detail.workflowJson,
            overlay: overlayOf(detail),
          ),
        );
    Navigator.of(context).popUntil((r) => r.isFirst);
    router.go('/flows');
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final s = detail.summary;
    final (icon, color, label) = statusStyle(hk, s.status);
    final overlay = overlayOf(detail);
    return ListView(
      padding: const EdgeInsets.all(HkTokens.s4),
      children: [
        Row(
          children: [
            Icon(icon, color: color),
            const SizedBox(width: HkTokens.s2),
            Text(label, style: text.titleMedium?.copyWith(color: color)),
          ],
        ),
        const SizedBox(height: HkTokens.s2),
        Text(
          '${formatTime(s.startedMs)} · ${formatDuration(s.durationMs)} · ${s.mode}'
          '${s.remote ? ' · on instance' : ''}',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
        Text(
          'Run ${s.id}',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
        if (s.error != null)
          Container(
            margin: const EdgeInsets.only(top: HkTokens.s3),
            padding: const EdgeInsets.all(HkTokens.s3),
            decoration: BoxDecoration(
              color: hk.err.withValues(alpha: 0.12),
              borderRadius: BorderRadius.circular(HkTokens.radiusCard),
              border: Border.all(color: hk.err),
            ),
            child: SelectableText(s.error!, style: text.bodySmall),
          ),
        const SizedBox(height: HkTokens.s3),
        FilledButton.icon(
          style: FilledButton.styleFrom(minimumSize: const Size(48, 48)),
          onPressed: detail.workflowJson == null
              ? null
              : () => _open(context, ref),
          icon: const Icon(Icons.account_tree_outlined),
          label: const Text('Open workflow with overlay'),
        ),
        const SizedBox(height: HkTokens.s4),
        Text('Nodes', style: text.titleSmall),
        if (overlay.nodes.isEmpty)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: HkTokens.s3),
            child: Text(
              'No node ran.',
              style: text.bodySmall?.copyWith(color: hk.textMuted),
            ),
          ),
        for (final e in overlay.nodes.entries)
          for (var i = 0; i < e.value.runs.length; i++)
            _NodeRunTile(
              name: e.value.runs.length > 1 ? '${e.key} (run ${i + 1})' : e.key,
              run: e.value.runs[i],
            ),
      ],
    );
  }
}

class _NodeRunTile extends StatelessWidget {
  const _NodeRunTile({required this.name, required this.run});

  final String name;
  final NodeRunInfo run;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final (icon, color, _) = statusStyle(hk, switch (run.status) {
      NodeStatus.success => 'success',
      NodeStatus.error => 'error',
      NodeStatus.waiting => 'waiting',
      NodeStatus.running => 'running',
    });
    return ExpansionTile(
      tilePadding: EdgeInsets.zero,
      minTileHeight: 56,
      leading: Icon(icon, color: color),
      title: Text(name, overflow: TextOverflow.ellipsis),
      subtitle: Text(
        '${itemsLabel(run.itemCount)} · ${formatDuration(run.durationMs)}',
      ),
      children: [
        if (run.error != null)
          Padding(
            padding: const EdgeInsets.only(bottom: HkTokens.s2),
            child: SelectableText(
              run.error!,
              style: text.bodySmall?.copyWith(color: hk.err),
            ),
          ),
        for (var o = 0; o < run.main.length; o++) ...[
          if (run.main.length > 1)
            Align(
              alignment: Alignment.centerLeft,
              child: Padding(
                padding: const EdgeInsets.only(left: HkTokens.s3),
                child: Text('Output $o', style: text.labelMedium),
              ),
            ),
          ItemsView(
            items: run.output(o),
            total: o < run.counts.length ? run.counts[o] : null,
            empty: 'No items on this output',
          ),
        ],
        if (run.main.isEmpty && run.error == null)
          Padding(
            padding: const EdgeInsets.all(HkTokens.s3),
            child: Text(
              'No output data',
              style: text.bodySmall?.copyWith(color: hk.textMuted),
            ),
          ),
      ],
    );
  }
}
