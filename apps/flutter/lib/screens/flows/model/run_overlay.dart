import 'dart:convert';

import 'workflow_model.dart';

/// Execution overlay of the flow canvas: what each node did in one run.
///
/// Pure Dart (no Flutter / FFI imports). The FFI event stream and the stored run
/// detail both fold into the same immutable [RunOverlay], so the canvas, the node
/// panel and the Runs screen read one shape.

enum RunEventKind {
  runStarted,
  nodeStarted,
  nodeFinished,
  runFinished,
  runError,
}

/// One step of a run, as the core reports it (mirrors `FlowRunEventDto`).
class RunEvent {
  const RunEvent({
    required this.kind,
    this.node,
    this.runIndex = 0,
    this.status,
    this.itemCount = 0,
    this.startedMs = 0,
    this.durationMs = 0,
    this.outputJson,
    this.truncated = false,
    this.error,
  });

  final RunEventKind kind;
  final String? node;
  final int runIndex;
  final String? status;
  final int itemCount;
  final int startedMs;
  final int durationMs;

  /// n8n `data.main`: `[[{json: ..}, ..], null, ..]`.
  final String? outputJson;
  final bool truncated;
  final String? error;
}

enum NodeStatus { running, success, error, waiting }

NodeStatus _nodeStatus(String? s) => switch (s) {
  'error' => NodeStatus.error,
  'waiting' => NodeStatus.waiting,
  'running' => NodeStatus.running,
  _ => NodeStatus.success,
};

/// One item as n8n shapes it: `{json: {...}, binary?: ..}`.
typedef FlowItem = Map<String, dynamic>;

/// `data.main` of a node run: one slot per output, `null` = output never produced.
typedef MainOutputs = List<List<FlowItem>?>;

MainOutputs parseMain(Object? raw) {
  if (raw is! List) return const [];
  return [
    for (final slot in raw)
      slot is List
          ? [
              for (final it in slot)
                if (it is Map) Map<String, dynamic>.from(it),
            ]
          : null,
  ];
}

/// One run of one node (loops run a node more than once).
class NodeRunInfo {
  const NodeRunInfo({
    required this.status,
    this.itemCount = 0,
    this.startedMs = 0,
    this.durationMs = 0,
    this.error,
    this.main = const [],
    this.counts = const [],
    this.truncated = false,
  });

  final NodeStatus status;

  /// True item count over all outputs (the shown lists may be cut).
  final int itemCount;
  final int startedMs;
  final int durationMs;
  final String? error;
  final MainOutputs main;

  /// True item count per output.
  final List<int> counts;
  final bool truncated;

  /// Items on output [i] (empty when not produced).
  List<FlowItem> output(int i) =>
      i < main.length ? main[i] ?? const [] : const [];
}

/// All runs of one node in a run.
class NodeOverlay {
  const NodeOverlay(this.runs);

  final List<NodeRunInfo> runs;

  NodeRunInfo get last => runs.last;

  /// Any failed run wins, then a still-running one, else the last status.
  NodeStatus get status {
    if (runs.any((r) => r.status == NodeStatus.error)) return NodeStatus.error;
    if (runs.any((r) => r.status == NodeStatus.running)) {
      return NodeStatus.running;
    }
    return runs.last.status;
  }

  /// Items of the latest run (what the badge shows).
  int get itemCount => runs.last.itemCount;

  int get durationMs => runs.fold(0, (a, r) => a + r.durationMs);

  String? get error {
    for (final r in runs) {
      if (r.error != null) return r.error;
    }
    return null;
  }

  NodeOverlay withRun(int index, NodeRunInfo run) {
    final next = List<NodeRunInfo>.of(runs);
    if (index < next.length) {
      next[index] = run;
    } else {
      next.add(run);
    }
    return NodeOverlay(List.unmodifiable(next));
  }
}

enum RunState { running, success, error, waiting }

RunState runStateOf(String? s) => switch (s) {
  'error' => RunState.error,
  'waiting' => RunState.waiting,
  'running' => RunState.running,
  _ => RunState.success,
};

class RunOverlay {
  const RunOverlay({
    required this.runId,
    this.nodes = const {},
    this.state = RunState.running,
    this.error,
    this.errorNode,
    this.startedMs = 0,
    this.durationMs = 0,
  });

  final String runId;
  final Map<String, NodeOverlay> nodes;
  final RunState state;
  final String? error;
  final String? errorNode;
  final int startedMs;
  final int durationMs;

  bool get running => state == RunState.running;

  NodeOverlay? of(String node) => nodes[node];

  /// Fold one event in; unrelated nodes keep their object identity.
  RunOverlay apply(RunEvent e) {
    switch (e.kind) {
      case RunEventKind.runStarted:
        return _copy(startedMs: e.startedMs, state: RunState.running);
      case RunEventKind.nodeStarted:
        final n = e.node;
        if (n == null) return this;
        final prev = nodes[n] ?? const NodeOverlay([]);
        return _withNode(
          n,
          prev.withRun(
            e.runIndex,
            NodeRunInfo(status: NodeStatus.running, startedMs: e.startedMs),
          ),
        );
      case RunEventKind.nodeFinished:
        final n = e.node;
        if (n == null) return this;
        final main = e.outputJson == null
            ? const <List<FlowItem>?>[]
            : parseMain(_tryDecode(e.outputJson!));
        final run = NodeRunInfo(
          status: _nodeStatus(e.status),
          itemCount: e.itemCount,
          startedMs: e.startedMs,
          durationMs: e.durationMs,
          error: e.error,
          main: main,
          counts: [for (final s in main) s?.length ?? 0],
          truncated: e.truncated,
        );
        final prev = nodes[n] ?? const NodeOverlay([]);
        return _withNode(n, prev.withRun(e.runIndex, run));
      case RunEventKind.runFinished:
        return _copy(state: runStateOf(e.status), durationMs: e.durationMs);
      case RunEventKind.runError:
        return _copy(
          state: RunState.error,
          error: e.error ?? 'Run failed',
          errorNode: e.node,
          durationMs: e.durationMs,
        );
    }
  }

  RunOverlay _withNode(String n, NodeOverlay o) =>
      _copy(nodes: {...nodes, n: o});

  RunOverlay _copy({
    Map<String, NodeOverlay>? nodes,
    RunState? state,
    String? error,
    String? errorNode,
    int? startedMs,
    int? durationMs,
  }) => RunOverlay(
    runId: runId,
    nodes: nodes ?? this.nodes,
    state: state ?? this.state,
    error: error ?? this.error,
    errorNode: errorNode ?? this.errorNode,
    startedMs: startedMs ?? this.startedMs,
    durationMs: durationMs ?? this.durationMs,
  );

  /// Overlay of a stored run (n8n `runData`, as `get_run` returns it; each run may
  /// carry `itemCounts` with the true per-output counts when its lists were cut).
  factory RunOverlay.fromRunData(
    String runId,
    String runDataJson, {
    String status = 'success',
    String? error,
    int startedMs = 0,
    int durationMs = 0,
  }) {
    final nodes = <String, NodeOverlay>{};
    final data = _tryDecode(runDataJson);
    String? errorNode;
    if (data is Map) {
      final ordered = <(int, String, List<NodeRunInfo>)>[];
      data.forEach((name, runs) {
        if (name is! String || runs is! List) return;
        final infos = [
          for (final r in runs)
            if (r is Map) _runInfo(r),
        ];
        if (infos.isEmpty) return;
        final first = runs.whereType<Map>().first;
        ordered.add((
          (first['executionIndex'] as num?)?.toInt() ?? 0,
          name,
          infos,
        ));
      });
      ordered.sort((a, b) => a.$1.compareTo(b.$1));
      for (final o in ordered) {
        nodes[o.$2] = NodeOverlay(List.unmodifiable(o.$3));
        if (errorNode == null && o.$3.any((r) => r.error != null)) {
          errorNode = o.$2;
        }
      }
    }
    return RunOverlay(
      runId: runId,
      nodes: nodes,
      state: runStateOf(status),
      error: error,
      errorNode: errorNode,
      startedMs: startedMs,
      durationMs: durationMs,
    );
  }

  /// Items flowing into [node]: what its upstream nodes emitted on the connected
  /// outputs (latest run of each). Empty for triggers and when nothing ran.
  List<FlowItem> inputItems(FlowWorkflow wf, String node) {
    final out = <FlowItem>[];
    for (final e in wf.connections.edges) {
      if (e.dst != node || e.type != 'main') continue;
      final up = nodes[e.src];
      if (up == null) continue;
      out.addAll(up.last.output(e.srcIndex));
    }
    return out;
  }

  /// Names of the nodes feeding [node] that produced data (for the Input tab header).
  List<String> inputSources(FlowWorkflow wf, String node) => [
    for (final e in wf.connections.edges)
      if (e.dst == node && e.type == 'main' && nodes.containsKey(e.src)) e.src,
  ];
}

NodeRunInfo _runInfo(Map r) {
  final main = parseMain((r['data'] as Map?)?['main']);
  final rawCounts = r['itemCounts'];
  final counts = rawCounts is List
      ? [for (final c in rawCounts) (c as num?)?.toInt() ?? 0]
      : [for (final s in main) s?.length ?? 0];
  final err = r['error'];
  return NodeRunInfo(
    status: _nodeStatus(r['executionStatus'] as String?),
    itemCount: counts.fold(0, (a, c) => a + c),
    startedMs: (r['startTime'] as num?)?.toInt() ?? 0,
    durationMs: (r['executionTime'] as num?)?.toInt() ?? 0,
    error: err is Map ? err['message'] as String? : null,
    main: main,
    counts: counts,
    truncated: r['truncated'] == true,
  );
}

Object? _tryDecode(String s) {
  try {
    return jsonDecode(s);
  } on FormatException {
    return null;
  }
}

// ----------------------------------------------------------------- pin data

/// n8n pinData items for a node: `[{json: {...}}]`. Binary payloads, pairing and
/// error markers are dropped; they are run artefacts, not data to freeze.
List<Map<String, dynamic>> toPinItems(List<FlowItem> items) => [
  for (final it in items)
    {
      'json': it['json'] is Map
          ? deepCopy(it['json'])! as Map<String, dynamic>
          : <String, dynamic>{},
    },
];

// ----------------------------------------------------------------- formatting

/// `850 ms`, `1.2 s`, `2 m 5 s`.
String formatDuration(int ms) {
  if (ms < 1000) return '$ms ms';
  if (ms < 60000) {
    final s = ms / 1000;
    return '${s >= 10 ? s.toStringAsFixed(0) : s.toStringAsFixed(1)} s';
  }
  final m = ms ~/ 60000;
  final s = (ms % 60000) ~/ 1000;
  return '$m m $s s';
}

/// `1 item` / `3 items`.
String itemsLabel(int n) => n == 1 ? '1 item' : '$n items';
