import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../ffi/ffi.dart' show rustInitErrorProvider;
import '../ffi/hk_gate.dart';
import '../theme/tokens.dart';

/// One node of the core's job tree (DESIGN §3.14).
class Job {
  Job({
    required this.id,
    required this.parent,
    required this.kind,
    required this.title,
    this.phase,
    this.progress,
    this.etaMs,
    this.state = 'running',
    this.metrics = const {},
    this.startedMs = 0,
    this.log,
    this.meta = const {},
  });

  factory Job.fromJson(Map<String, dynamic> j) {
    final k = j['kind'];
    return Job(
      id: (j['id'] as num).toInt(),
      parent: (j['parent'] as num?)?.toInt(),
      kind: k is String
          ? k
          : (k is Map
                ? (k.values.firstOrNull?.toString() ?? 'other')
                : 'other'),
      title: j['title'] as String? ?? '',
      phase: j['phase'] as String?,
      progress: _progress(j['progress']),
      etaMs: (j['eta_ms'] as num?)?.toInt(),
      state: _state(j['state']),
      metrics: (j['metrics'] is Map)
          ? (j['metrics'] as Map).cast<String, dynamic>()
          : const {},
      startedMs: (j['started_at_ms'] as num?)?.toInt() ?? 0,
      meta: (j['meta'] is Map)
          ? (j['meta'] as Map).cast<String, dynamic>()
          : const {},
    );
  }

  final int id;
  final int? parent;
  final String kind;
  final String title;
  String? phase;

  /// 0..1, or null while indeterminate.
  double? progress;
  int? etaMs;
  String state;
  Map<String, dynamic> metrics;
  final int startedMs;
  String? log;
  final Map<String, dynamic> meta;

  bool get running => state == 'running' || state == 'cancelling';
}

double? _progress(Object? p) {
  if (p is Map && p['kind'] == 'determinate') {
    return (p['value'] as num?)?.toDouble();
  }
  return null;
}

String _state(Object? s) => s is String
    ? s
    : (s is Map ? (s.keys.firstOrNull?.toString() ?? 'failed') : 'running');

/// Live job map fed by `jobs.list` + the `jobs` event stream.
class JobsNotifier extends Notifier<Map<int, Job>> {
  StreamSubscription<Map<String, dynamic>>? _sub;

  @override
  Map<int, Job> build() {
    ref.onDispose(() => _sub?.cancel());
    if (ref.watch(rustInitErrorProvider) != null) return const {};
    _start();
    return const {};
  }

  Future<void> _start() async {
    try {
      final list = await hkList('jobs.list');
      state = {for (final j in list) (j['id'] as num).toInt(): Job.fromJson(j)};
    } catch (_) {
      // Core without job tracking: the Activity view stays empty.
    }
    _sub = hkStream('jobs').listen(_on, onError: (_) {});
  }

  void _on(Map<String, dynamic> ev) {
    final next = Map<int, Job>.of(state);
    final type = ev['type'] as String? ?? '';
    if (type == 'job/started') {
      final j = Job.fromJson((ev['job'] as Map).cast<String, dynamic>());
      next[j.id] = j;
    } else {
      final id = (ev['id'] as num?)?.toInt();
      final j = id == null ? null : next[id];
      if (j == null) return;
      switch (type) {
        case 'job/phase':
          j.phase = ev['phase'] as String?;
        case 'job/progress':
          j.progress = _progress(ev['progress']);
          j.etaMs = (ev['eta_ms'] as num?)?.toInt();
        case 'job/log':
          j.log = ev['line'] as String?;
        case 'job/metrics':
          j.metrics =
              (ev['metrics'] as Map?)?.cast<String, dynamic>() ?? const {};
        case 'job/cancel-requested':
          j.state = 'cancelling';
        case 'job/cancelled':
          j.state = 'cancelled';
        case 'job/finished':
          j.state = 'finished';
          j.progress = 1;
        case 'job/failed':
          j.state = 'failed';
          j.log = ev['error'] as String?;
      }
    }
    // Keep the last 200 jobs.
    if (next.length > 200) {
      final done =
          next.values.where((j) => !j.running).map((j) => j.id).toList()
            ..sort();
      for (final id in done.take(next.length - 200)) {
        next.remove(id);
      }
    }
    state = next;
  }

  Future<void> cancel(int id) async => hk('jobs.cancel', {'id': id});
  Future<void> cancelAll() async => hk('jobs.cancel_all');
  void clearFinished() => state = {
    for (final e in state.entries)
      if (e.value.running) e.key: e.value,
  };
}

final jobsProvider = NotifierProvider<JobsNotifier, Map<int, Job>>(
  JobsNotifier.new,
);

/// Running root jobs (for the top-bar indicator).
final runningRootsProvider = Provider<int>(
  (ref) => ref
      .watch(jobsProvider)
      .values
      .where((j) => j.parent == null && j.running)
      .length,
);

IconData _kindIcon(String k) => switch (k) {
  'agent_turn' => Icons.smart_toy_outlined,
  'step' => Icons.linear_scale,
  'tool' => Icons.build_outlined,
  'subagent' => Icons.account_tree_outlined,
  'flow_execution' => Icons.account_tree,
  'render' => Icons.movie_creation_outlined,
  'transcribe' => Icons.subtitles_outlined,
  'download' => Icons.download_outlined,
  'index' => Icons.manage_search,
  _ => Icons.bolt_outlined,
};

String _eta(int? ms) {
  if (ms == null) return '';
  final s = (ms / 1000).round();
  return 'ETA ${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}

/// Rail / top-bar button: spinner badge while anything runs; opens the Activity center.
class ActivityButton extends ConsumerWidget {
  const ActivityButton({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final n = ref.watch(runningRootsProvider);
    return IconButton(
      tooltip: 'Activity',
      onPressed: () => showActivity(context),
      icon: n == 0
          ? const Icon(Icons.timelapse)
          : Badge.count(count: n, child: const Icon(Icons.timelapse)),
    );
  }
}

/// Right-side sheet listing the job tree with progress, ETA, live metrics and cancel.
Future<void> showActivity(BuildContext context) => showGeneralDialog<void>(
  context: context,
  barrierDismissible: true,
  barrierLabel: 'Activity',
  pageBuilder: (ctx, _, _) => Align(
    alignment: Alignment.centerRight,
    child: Material(
      elevation: 8,
      color: ctx.hk.surface,
      child: SizedBox(
        width: MediaQuery.sizeOf(ctx).width.clamp(320, 460).toDouble(),
        height: double.infinity,
        child: const SafeArea(child: ActivityPanel()),
      ),
    ),
  ),
);

class ActivityPanel extends ConsumerWidget {
  const ActivityPanel({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final jobs = ref.watch(jobsProvider);
    final n = ref.read(jobsProvider.notifier);
    final roots =
        jobs.values
            .where((j) => j.parent == null || !jobs.containsKey(j.parent))
            .toList()
          ..sort((a, b) => b.id.compareTo(a.id));
    final hk = context.hk;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            HkTokens.s4,
            HkTokens.s3,
            HkTokens.s2,
            HkTokens.s2,
          ),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  'Activity',
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
              TextButton(
                onPressed: n.clearFinished,
                child: const Text('Clear done'),
              ),
              TextButton.icon(
                onPressed: jobs.values.any((j) => j.running)
                    ? n.cancelAll
                    : null,
                icon: const Icon(Icons.stop_circle_outlined),
                label: const Text('Cancel all'),
              ),
            ],
          ),
        ),
        Divider(height: 1, color: hk.border),
        Expanded(
          child: roots.isEmpty
              ? Center(
                  child: Text(
                    'Nothing running',
                    style: TextStyle(color: hk.textMuted),
                  ),
                )
              : ListView(
                  padding: const EdgeInsets.symmetric(vertical: HkTokens.s2),
                  children: [
                    for (final r in roots) ..._tree(context, ref, jobs, r, 0),
                  ],
                ),
        ),
      ],
    );
  }

  List<Widget> _tree(
    BuildContext context,
    WidgetRef ref,
    Map<int, Job> all,
    Job j,
    int depth,
  ) {
    final kids = all.values.where((c) => c.parent == j.id).toList()
      ..sort((a, b) => a.id.compareTo(b.id));
    return [
      JobRow(job: j, depth: depth),
      if (depth < 4)
        for (final k in kids) ..._tree(context, ref, all, k, depth + 1),
    ];
  }
}

class JobRow extends ConsumerWidget {
  const JobRow({super.key, required this.job, required this.depth});
  final Job job;
  final int depth;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final m = job.metrics;
    final bits = <String>[
      if (job.phase != null) job.phase!,
      if (m['tokens_per_s'] is num && (m['tokens_per_s'] as num) > 0)
        '${m['tokens_per_s']} tok/s',
      if (m['ttft_ms'] is num) 'TTFT ${m['ttft_ms']} ms',
      if (m['tokens_out'] is num) '${m['tokens_out']} tok',
      if (m['context_tokens'] is num) 'ctx ${m['context_tokens']}',
      if (job.etaMs != null) _eta(job.etaMs),
    ];
    final color = switch (job.state) {
      'failed' => hk.err,
      'cancelled' => hk.textMuted,
      'finished' => hk.ok,
      _ => hk.accent,
    };
    return Padding(
      padding: EdgeInsets.fromLTRB(
        HkTokens.s3 + depth * 16.0,
        4,
        HkTokens.s2,
        4,
      ),
      child: Row(
        children: [
          Icon(_kindIcon(job.kind), size: 18, color: color),
          const SizedBox(width: HkTokens.s2),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Text(job.title, maxLines: 1, overflow: TextOverflow.ellipsis),
                if (bits.isNotEmpty)
                  Text(
                    bits.join(' · '),
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 12, color: hk.textMuted),
                  ),
                if (job.running) ...[
                  const SizedBox(height: 4),
                  LinearProgressIndicator(value: job.progress, minHeight: 3),
                ] else if (job.log != null)
                  Text(
                    job.log!,
                    maxLines: 2,
                    overflow: TextOverflow.ellipsis,
                    style: TextStyle(fontSize: 12, color: color),
                  ),
              ],
            ),
          ),
          if (job.running)
            IconButton(
              tooltip: job.kind == 'tool' ? 'Skip this tool' : 'Cancel',
              icon: Icon(
                job.kind == 'tool'
                    ? Icons.skip_next
                    : Icons.stop_circle_outlined,
                size: 20,
              ),
              onPressed: job.state == 'cancelling'
                  ? null
                  : () => ref.read(jobsProvider.notifier).cancel(job.id),
            ),
        ],
      ),
    );
  }
}

/// Live LLM metrics of the running turn of `sessionId` (latest step), or null when idle.
final sessionLiveProvider = Provider.family<Job?, String>((ref, sessionId) {
  final jobs = ref.watch(jobsProvider);
  final turn = jobs.values
      .where(
        (j) =>
            j.kind == 'agent_turn' &&
            j.running &&
            j.meta['session_id'] == sessionId,
      )
      .fold<Job?>(null, (a, j) => a == null || j.id > a.id ? j : a);
  if (turn == null) return null;
  final step = jobs.values
      .where((j) => j.parent == turn.id && j.kind == 'step')
      .fold<Job?>(null, (a, j) => a == null || j.id > a.id ? j : a);
  return step ?? turn;
});

/// One-line live status: phase · tok/s · TTFT · tokens · context.
String liveLine(Job j) {
  final m = j.metrics;
  return [
    (m['phase'] ?? j.phase ?? 'working').toString(),
    if (m['tokens_per_s'] is num && (m['tokens_per_s'] as num) > 0)
      '${m['tokens_per_s']} tok/s',
    if (m['ttft_ms'] is num) 'TTFT ${m['ttft_ms']} ms',
    if (m['tokens_out'] is num) '${m['tokens_out']} tok',
    if (m['context_tokens'] is num) 'ctx ${m['context_tokens']}',
  ].join(' · ');
}
