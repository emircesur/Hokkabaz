import 'dart:convert';
import 'dart:io';

import 'package:path_provider/path_provider.dart';

import 'model/workflow_model.dart';

/// Row in the flows list.
class FlowFile {
  const FlowFile({required this.id, required this.name, required this.modified});
  final String id;
  final String name;
  final DateTime modified;
}

/// Workflows as plain n8n JSON files (`<dir>/<id>.json`).
///
/// Stopgap until hk-store / REST persistence lands (DESIGN §3.10); the file
/// content is already the final format, so migrating is a straight import.
class FlowStore {
  FlowStore(this.dir);

  final Directory dir;

  static Future<FlowStore> open() async {
    final base = await getApplicationSupportDirectory();
    return FlowStore(Directory('${base.path}${Platform.pathSeparator}flows'));
  }

  File _file(String id) => File('${dir.path}${Platform.pathSeparator}$id.json');

  Future<List<FlowFile>> list() async {
    if (!await dir.exists()) return const [];
    final out = <FlowFile>[];
    await for (final e in dir.list()) {
      if (e is! File || !e.path.endsWith('.json')) continue;
      final id = e.uri.pathSegments.last.replaceFirst(RegExp(r'\.json$'), '');
      var name = id;
      try {
        final j = jsonDecode(await e.readAsString());
        if (j is Map && j['name'] is String && (j['name'] as String).isNotEmpty) {
          name = j['name'] as String;
        }
      } catch (_) {
        // Corrupt file stays listed under its id so the user can delete it.
      }
      out.add(FlowFile(id: id, name: name, modified: (await e.stat()).modified));
    }
    out.sort((a, b) => a.name.toLowerCase().compareTo(b.name.toLowerCase()));
    return out;
  }

  Future<FlowWorkflow> load(String id) async =>
      FlowWorkflow.parse(await _file(id).readAsString());

  Future<void> save(String id, FlowWorkflow wf) async {
    await dir.create(recursive: true);
    // Write-then-rename so a crash never leaves half a workflow.
    final tmp = File('${_file(id).path}.tmp');
    await tmp.writeAsString(wf.toJsonString());
    await tmp.rename(_file(id).path);
  }

  Future<FlowFile> create(String name, {FlowWorkflow? from}) async {
    final id = 'flow_${DateTime.now().microsecondsSinceEpoch.toRadixString(36)}';
    final wf = (from ?? FlowWorkflow.empty()).withName(name);
    await save(id, wf);
    return FlowFile(id: id, name: name, modified: DateTime.now());
  }

  Future<void> rename(String id, String name) async =>
      save(id, (await load(id)).withName(name));

  Future<void> delete(String id) async {
    final f = _file(id);
    if (await f.exists()) await f.delete();
  }
}
