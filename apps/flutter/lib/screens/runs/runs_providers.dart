import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart' show backendErrorProvider;
import '../../src/rust/api/flow_runs.dart' as api;
import '../../src/rust/dto.dart';
import '../flows/model/run_overlay.dart';
import '../instances/instances_providers.dart' show instanceBookProvider;

/// Past runs of the active core, newest first. Re-reads when the instance
/// changes; invalidate to refresh.
final runListProvider = FutureProvider.autoDispose<List<FlowRunSummaryDto>>((
  ref,
) async {
  if (ref.watch(backendErrorProvider) != null) return const [];
  ref.watch(instanceBookProvider);
  return api.listRuns(limit: 100);
});

final runDetailProvider = FutureProvider.autoDispose
    .family<FlowRunDetailDto, String>((ref, id) => api.getRun(runId: id));

/// True while a remote instance is the active core: only then can a workflow be
/// activated (triggers need a process that stays up).
final remoteConnectedProvider = Provider<bool>((ref) {
  final book = ref.watch(instanceBookProvider).value ?? const [];
  return book.any((i) => i.kind == InstanceKindDto.remote && i.connected);
});

/// A run the Runs screen wants shown on the canvas. The Flows screen takes it
/// (sets it back to null) when it builds.
class PendingFlowOpen {
  const PendingFlowOpen({
    required this.flowId,
    required this.workflowJson,
    required this.overlay,
  });

  /// Local flow file the run belongs to, if known.
  final String? flowId;

  /// The workflow as it was when the run started; imported as a copy when
  /// [flowId] is not on this device.
  final String? workflowJson;
  final RunOverlay overlay;
}

class PendingFlowOpenController extends Notifier<PendingFlowOpen?> {
  @override
  PendingFlowOpen? build() => null;

  void set(PendingFlowOpen? v) => state = v;
}

final pendingFlowOpenProvider =
    NotifierProvider<PendingFlowOpenController, PendingFlowOpen?>(
      PendingFlowOpenController.new,
    );
