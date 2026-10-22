import '../../src/rust/api/flow_runs.dart' as api;
import '../../src/rust/dto.dart';
import 'model/run_overlay.dart';
import 'run_controller.dart';

/// The real core behind [FlowRunController].
FlowRunController ffiRunController() =>
    FlowRunController(start: _start, events: _events);

Future<String> _start({
  required String workflowJson,
  required bool test,
  String? triggerNode,
}) => api.runWorkflow(
  workflowJson: workflowJson,
  mode: test ? FlowRunModeDto.test : FlowRunModeDto.manual,
  triggerNode: triggerNode,
);

Stream<RunEvent> _events(String runId) =>
    api.runEvents(runId: runId).map(toRunEvent);

RunEvent toRunEvent(FlowRunEventDto d) => RunEvent(
  kind: switch (d.kind) {
    FlowRunEventKind.runStarted => RunEventKind.runStarted,
    FlowRunEventKind.nodeStarted => RunEventKind.nodeStarted,
    FlowRunEventKind.nodeFinished => RunEventKind.nodeFinished,
    FlowRunEventKind.runFinished => RunEventKind.runFinished,
    FlowRunEventKind.runError => RunEventKind.runError,
  },
  node: d.node,
  runIndex: d.runIndex,
  status: d.status,
  itemCount: d.itemCount,
  startedMs: d.startedMs,
  durationMs: d.durationMs,
  outputJson: d.outputJson,
  truncated: d.truncated,
  error: d.error,
);

/// Overlay of a stored run.
RunOverlay overlayOf(FlowRunDetailDto d) => RunOverlay.fromRunData(
  d.summary.id,
  d.runDataJson,
  status: d.summary.status,
  error: d.summary.error,
  startedMs: d.summary.startedMs,
  durationMs: d.summary.durationMs,
);
