import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart' show rustInitErrorProvider;
import '../../src/rust/api/flows.dart' as api;
import 'model/node_types.dart';

/// Node catalog from the Rust flow registry, or the reason it is missing. An
/// empty catalog is not fatal: flows still open, every node shows as unsupported.
class NodeTypesResult {
  NodeTypesResult(this.list, this.error)
    : byType = {for (final t in list) t.typeName: t};

  final List<NodeTypeInfo> list;
  final Map<String, NodeTypeInfo> byType;
  final String? error;
}

final nodeTypesProvider = Provider<NodeTypesResult>((ref) {
  final initError = ref.watch(rustInitErrorProvider);
  if (initError != null) {
    return NodeTypesResult(const [], 'Rust unavailable: $initError');
  }
  try {
    return NodeTypesResult([
      for (final t in api.listNodeTypes())
        NodeTypeInfo.fromFields(
          typeName: t.typeName,
          versions: t.versions.toList(),
          defaultVersion: t.defaultVersion,
          displayName: t.displayName,
          description: t.description,
          group: t.group,
          inputs: t.inputs,
          outputs: t.outputs,
          inputsExpr: t.inputsExpr,
          outputsExpr: t.outputsExpr,
          outputNames: t.outputNames,
          propertiesJson: t.propertiesJson,
        ),
    ], null);
  } catch (e) {
    return NodeTypesResult(const [], e.toString());
  }
});

/// Engine-side structural check (`hk_flow::Workflow::validate` + unsupported
/// nodes). Returns an empty list when Rust is unavailable.
List<String> validateWithEngine(String json) {
  try {
    return api.validateWorkflow(json: json);
  } catch (_) {
    return const [];
  }
}
