import 'dart:async';

import 'package:flutter/foundation.dart';

import 'model/run_overlay.dart';
import 'model/workflow_model.dart';

/// Starts a run in the active core and returns its id.
typedef RunStarter = Future<String> Function({
  required String workflowJson,
  required bool test,
  String? triggerNode,
});

/// Event feed of one run (replays from the start, ends at the terminal event).
typedef RunEventSource = Stream<RunEvent> Function(String runId);

/// Owns the overlay of the run shown on the canvas: starts runs, folds the event
/// stream into a [RunOverlay], and can show a stored run instead.
///
/// The core is injected, so the logic is testable without the FFI.
class FlowRunController extends ChangeNotifier {
  FlowRunController({required this.start, required this.events});

  final RunStarter start;
  final RunEventSource events;

  RunOverlay? _overlay;
  String? _error;
  bool _starting = false;
  StreamSubscription<RunEvent>? _sub;
  int _gen = 0;

  RunOverlay? get overlay => _overlay;

  /// Start failed (bad workflow, no trigger, core unreachable); runs that fail
  /// while executing are in [overlay] instead.
  String? get error => _error;

  bool get running => _starting || (_overlay?.running ?? false);

  /// Test / manual run of [wf]. [flowId] becomes the workflow `id` of the run (the
  /// file name; the canvas JSON itself carries none). [test] honours pinned data,
  /// otherwise nodes really execute. [triggerNode] null = the Manual Trigger.
  Future<void> run(
    FlowWorkflow wf, {
    required String flowId,
    bool test = true,
    String? triggerNode,
  }) async {
    if (running) return;
    final gen = ++_gen;
    await _sub?.cancel();
    _sub = null;
    _error = null;
    _starting = true;
    notifyListeners();
    try {
      final json = wf.withField('id', flowId).toJsonString(pretty: false);
      final id = await start(
        workflowJson: json,
        test: test,
        triggerNode: triggerNode,
      );
      if (gen != _gen) return;
      _overlay = RunOverlay(runId: id);
      _starting = false;
      notifyListeners();
      _sub = events(id).listen(
        (e) {
          if (gen != _gen) return;
          _overlay = _overlay?.apply(e);
          notifyListeners();
        },
        onError: (Object e) {
          if (gen != _gen) return;
          _overlay = _overlay?.apply(
            RunEvent(kind: RunEventKind.runError, error: e.toString()),
          );
          notifyListeners();
        },
      );
    } catch (e) {
      if (gen != _gen) return;
      _starting = false;
      _error = e.toString();
      notifyListeners();
    }
  }

  /// Show a stored run (from the Runs screen) instead of a live one.
  void show(RunOverlay overlay) {
    _gen++;
    unawaited(_sub?.cancel());
    _sub = null;
    _starting = false;
    _error = null;
    _overlay = overlay;
    notifyListeners();
  }

  void clear() {
    _gen++;
    unawaited(_sub?.cancel());
    _sub = null;
    _starting = false;
    _error = null;
    _overlay = null;
    notifyListeners();
  }

  @override
  void dispose() {
    _gen++;
    unawaited(_sub?.cancel());
    super.dispose();
  }
}
