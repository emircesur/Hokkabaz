import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../../ffi/ffi.dart' show backendErrorProvider;
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import '../flows/model/run_overlay.dart';
import 'run_detail_screen.dart';
import 'run_format.dart';
import 'runs_providers.dart';

/// Icon, color and label of a run / node status string.
(IconData, Color, String) statusStyle(HkTokens hk, String status) =>
    switch (status) {
      'success' => (Icons.check_circle, hk.ok, 'Success'),
      'error' => (Icons.error, hk.err, 'Error'),
      'waiting' => (Icons.pause_circle, hk.warn, 'Waiting'),
      'running' => (Icons.sync, hk.info, 'Running'),
      'canceled' => (Icons.cancel, hk.textMuted, 'Canceled'),
      _ => (Icons.help_outline, hk.textMuted, status),
    };

/// DESIGN §4.2 "Runs": executions of the active core (this device, or the
/// connected instance), newest first. Tap one for per-node data and to reopen
/// its workflow with the execution overlay.
class RunsScreen extends ConsumerStatefulWidget {
  const RunsScreen({super.key});

  @override
  ConsumerState<RunsScreen> createState() => _RunsScreenState();
}

class _RunsScreenState extends ConsumerState<RunsScreen> {
  String? _status;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final backendError = ref.watch(backendErrorProvider);
    final runs = ref.watch(runListProvider);
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            HkTokens.s4,
            HkTokens.s3,
            HkTokens.s2,
            0,
          ),
          child: Row(
            children: [
              Expanded(child: Text('Runs', style: text.titleLarge)),
              IconButton(
                tooltip: 'Refresh',
                icon: const Icon(Icons.refresh),
                onPressed: () => ref.invalidate(runListProvider),
              ),
            ],
          ),
        ),
        Padding(
          padding: const EdgeInsets.symmetric(horizontal: HkTokens.s4),
          child: Wrap(
            spacing: HkTokens.s2,
            children: [
              for (final (value, label) in const [
                (null, 'All'),
                ('success', 'Success'),
                ('error', 'Error'),
              ])
                ChoiceChip(
                  materialTapTargetSize: MaterialTapTargetSize.padded,
                  label: Text(label),
                  selected: _status == value,
                  onSelected: (_) => setState(() => _status = value),
                ),
            ],
          ),
        ),
        const SizedBox(height: HkTokens.s2),
        Divider(height: 1, color: hk.border),
        Expanded(
          child: backendError != null
              ? _Message(icon: Icons.error_outline, text: backendError)
              : runs.when(
                  loading: () =>
                      const Center(child: CircularProgressIndicator()),
                  error: (e, _) => _Message(
                    icon: Icons.error_outline,
                    text: describeError(e),
                    action: TextButton(
                      onPressed: () => ref.invalidate(runListProvider),
                      child: const Text('Retry'),
                    ),
                  ),
                  data: (all) {
                    final list = [
                      for (final r in all)
                        if (_status == null || r.status == _status) r,
                    ];
                    if (list.isEmpty) {
                      return _Message(
                        icon: Icons.play_circle_outline,
                        text: all.isEmpty
                            ? 'No runs yet. Open a flow and press Test workflow.'
                            : 'No runs with this status.',
                        action: all.isEmpty
                            ? FilledButton(
                                onPressed: () => context.go('/flows'),
                                child: const Text('Open Flows'),
                              )
                            : null,
                      );
                    }
                    return RefreshIndicator(
                      onRefresh: () => ref.refresh(runListProvider.future),
                      child: ListView.separated(
                        itemCount: list.length,
                        separatorBuilder: (_, _) =>
                            Divider(height: 1, color: hk.border),
                        itemBuilder: (_, i) => _RunTile(run: list[i]),
                      ),
                    );
                  },
                ),
        ),
      ],
    );
  }
}

class _RunTile extends StatelessWidget {
  const _RunTile({required this.run});

  final FlowRunSummaryDto run;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final (icon, color, label) = statusStyle(hk, run.status);
    final dur = run.finishedMs == null && run.status == 'running'
        ? 'running'
        : formatDuration(run.durationMs);
    return ListTile(
      minTileHeight: 64,
      leading: Tooltip(
        message: label,
        child: Icon(icon, color: color),
      ),
      title: Text(run.workflowName, overflow: TextOverflow.ellipsis),
      subtitle: Text(
        [
          run.mode,
          relativeTime(run.startedMs),
          dur,
          if (run.remote) 'instance',
          if (run.error != null) run.error!,
        ].join(' · '),
        maxLines: 2,
        overflow: TextOverflow.ellipsis,
      ),
      trailing: const Icon(Icons.chevron_right),
      onTap: () => Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => RunDetailScreen(runId: run.id),
        ),
      ),
    );
  }
}

class _Message extends StatelessWidget {
  const _Message({required this.icon, required this.text, this.action});

  final IconData icon;
  final String text;
  final Widget? action;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(HkTokens.s4),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(icon, size: 40, color: hk.textMuted),
            const SizedBox(height: HkTokens.s3),
            Text(
              text,
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodyMedium,
            ),
            if (action != null) ...[
              const SizedBox(height: HkTokens.s3),
              action!,
            ],
          ],
        ),
      ),
    );
  }
}
