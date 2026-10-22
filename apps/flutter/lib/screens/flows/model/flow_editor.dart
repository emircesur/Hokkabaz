import 'package:flutter/foundation.dart';

import 'n8n_clipboard.dart';
import 'node_types.dart';
import 'workflow_model.dart';

/// Editing session over one [FlowWorkflow]: ops, selection, undo/redo.
///
/// Every op swaps in a new immutable workflow, so history is a list of
/// snapshots. Drags use [beginDrag]/[dragBy]/[endDrag] so one gesture = one
/// undo step.
class FlowEditor extends ChangeNotifier {
  FlowEditor([FlowWorkflow? wf]) : _wf = wf ?? FlowWorkflow.empty() {
    _saved = _wf;
  }

  static const maxHistory = 100;

  FlowWorkflow _wf;
  late FlowWorkflow _saved;
  final List<FlowWorkflow> _undo = [];
  final List<FlowWorkflow> _redo = [];
  final Set<String> _selection = {};
  FlowEdge? _selectedEdge;

  // A burst of edits with one key (typing in a field) is one undo step.
  String? _coalesceKey;
  DateTime _coalesceAt = DateTime.fromMillisecondsSinceEpoch(0);

  FlowWorkflow? _dragBase;
  Map<String, Pos> _dragStart = const {};

  FlowWorkflow get workflow => _wf;
  bool get canUndo => _undo.isNotEmpty;
  bool get canRedo => _redo.isNotEmpty;

  /// Differs from the last [markSaved] / [load] state.
  bool get dirty => !identical(_wf, _saved);

  Set<String> get selection => Set.unmodifiable(_selection);
  FlowEdge? get selectedEdge => _selectedEdge;

  /// The one selected node, or null for none / several.
  FlowNode? get selectedNode =>
      _selection.length == 1 ? _wf.node(_selection.first) : null;

  /// Replace the document (open / import). Clears history and selection.
  void load(FlowWorkflow wf) {
    _wf = wf;
    _saved = wf;
    _undo.clear();
    _redo.clear();
    _selection.clear();
    _selectedEdge = null;
    _dragBase = null;
    notifyListeners();
  }

  void markSaved() {
    _saved = _wf;
    notifyListeners();
  }

  // history

  void _commit(FlowWorkflow next) {
    _coalesceKey = null;
    _push(_wf);
    _redo.clear();
    _wf = next;
    notifyListeners();
  }

  void _push(FlowWorkflow w) {
    _undo.add(w);
    if (_undo.length > maxHistory) _undo.removeAt(0);
  }

  bool undo() {
    if (_undo.isEmpty) return false;
    _redo.add(_wf);
    _wf = _undo.removeLast();
    _afterJump();
    return true;
  }

  bool redo() {
    if (_redo.isEmpty) return false;
    _undo.add(_wf);
    _wf = _redo.removeLast();
    _afterJump();
    return true;
  }

  void _afterJump() {
    _dragBase = null;
    _coalesceKey = null;
    _selection.removeWhere((n) => !_wf.hasNode(n));
    if (_selectedEdge != null && !_wf.connections.contains(_selectedEdge!)) {
      _selectedEdge = null;
    }
    notifyListeners();
  }

  // selection

  void select(Iterable<String> names, {bool additive = false}) {
    if (!additive) _selection.clear();
    _selection.addAll(names.where(_wf.hasNode));
    _selectedEdge = null;
    notifyListeners();
  }

  void toggleSelect(String name) {
    if (!_selection.remove(name) && _wf.hasNode(name)) _selection.add(name);
    _selectedEdge = null;
    notifyListeners();
  }

  void selectEdge(FlowEdge? e) {
    _selection.clear();
    _selectedEdge = e;
    notifyListeners();
  }

  void selectAll() => select(_wf.nodes.map((n) => n.name));

  void clearSelection() {
    if (_selection.isEmpty && _selectedEdge == null) return;
    _selection.clear();
    _selectedEdge = null;
    notifyListeners();
  }

  // nodes

  /// Add [type] at [at]; connects from output [connectFromIndex] of [connectFrom] when given.
  /// Returns the new node's name.
  String addNode(
    NodeTypeInfo type,
    Pos at, {
    num? version,
    String? connectFrom,
    int connectFromIndex = 0,
  }) => addRawNode(
    type: type.typeName,
    version: version ?? type.defaultVersion,
    baseName: type.displayName,
    at: at,
    connectFrom: connectFrom,
    connectFromIndex: connectFromIndex,
  );

  /// Add a node by raw type string (types missing from the registry become
  /// unsupported placeholders).
  String addRawNode({
    required String type,
    required num version,
    required String baseName,
    required Pos at,
    String? connectFrom,
    int connectFromIndex = 0,
  }) {
    final name = uniqueName(baseName, _wf.nodes.map((n) => n.name));
    final node = FlowNode.create(
      id: newNodeId(),
      name: name,
      type: type,
      typeVersion: version,
      position: at,
    );
    var next = _wf.copyWith(nodes: [..._wf.nodes, node]);
    if (connectFrom != null && _wf.hasNode(connectFrom)) {
      final conns = next.connections.clone()
        ..add(
          FlowEdge(
            src: connectFrom,
            srcIndex: connectFromIndex,
            dst: name,
            dstIndex: 0,
          ),
        );
      next = next.copyWith(connections: conns);
    }
    _commit(next);
    _selection
      ..clear()
      ..add(name);
    _selectedEdge = null;
    notifyListeners();
    return name;
  }

  void deleteNodes(Iterable<String> names) {
    final gone = names.where(_wf.hasNode).toSet();
    if (gone.isEmpty) return;
    final conns = _wf.connections.clone();
    for (final n in gone) {
      conns.removeNode(n);
    }
    var next = _wf.copyWith(
      nodes: [
        for (final n in _wf.nodes)
          if (!gone.contains(n.name)) n,
      ],
      connections: conns,
    );
    if (_wf.pinData.keys.any(gone.contains)) {
      next = next.withField('pinData', {
        for (final e in _wf.pinData.entries)
          if (!gone.contains(e.key)) e.key: e.value,
      });
    }
    _commit(next);
    _selection.removeAll(gone);
    notifyListeners();
  }

  /// Delete selected edge or nodes (Delete key).
  void deleteSelection() {
    final e = _selectedEdge;
    if (e != null) {
      disconnect(e);
      _selectedEdge = null;
      notifyListeners();
    } else {
      deleteNodes(_selection.toList());
    }
  }

  void moveNodes(Map<String, Pos> to) {
    _commit(_withPositions(_wf, to));
  }

  FlowWorkflow _withPositions(FlowWorkflow w, Map<String, Pos> to) =>
      w.copyWith(
        nodes: [
          for (final n in w.nodes)
            to.containsKey(n.name) ? n.withPosition(to[n.name]!) : n,
        ],
      );

  void beginDrag(Iterable<String> names) {
    _dragBase = _wf;
    _dragStart = {
      for (final n in names)
        if (_wf.node(n) != null) n: _wf.node(n)!.position,
    };
  }

  /// Offset from the drag start, in canvas units. No history entry yet.
  void dragBy(double dx, double dy) {
    if (_dragBase == null) return;
    _wf = _withPositions(_dragBase!, {
      for (final e in _dragStart.entries)
        e.key: (x: e.value.x + dx, y: e.value.y + dy),
    });
    notifyListeners();
  }

  void endDrag() {
    final base = _dragBase;
    _dragBase = null;
    if (base == null) return;
    var moved = false;
    for (final e in _dragStart.entries) {
      final a = base.node(e.key)?.position;
      final b = _wf.node(e.key)?.position;
      if (a != b) moved = true;
    }
    if (moved) {
      _push(base);
      _redo.clear();
    } else {
      _wf = base;
    }
    notifyListeners();
  }

  // pin data

  bool isPinned(String node) => _wf.pinData.containsKey(node);

  /// Freeze [node]'s output to [items] (n8n `pinData`: `{node: [{json: ..}]}`). Runs in
  /// test mode emit these instead of executing the node.
  void pinItems(String node, List<Map<String, dynamic>> items) {
    if (!_wf.hasNode(node)) return;
    _commit(_wf.withField('pinData', {..._wf.pinData, node: deepCopy(items)}));
  }

  void unpin(String node) {
    if (!isPinned(node)) return;
    final rest = {
      for (final e in _wf.pinData.entries)
        if (e.key != node) e.key: e.value,
    };
    _commit(
      rest.isEmpty
          ? _wf.withoutField('pinData')
          : _wf.withField('pinData', rest),
    );
  }

  /// Mirror the instance's activation flag into the document.
  void setActive(bool active) {
    if (_wf.active == active) return;
    _commit(_wf.withField('active', active));
  }

  /// Rename and rewrite connections + pinData. Returns false when the name is
  /// empty or taken.
  bool renameNode(String from, String to) {
    final name = to.trim();
    if (name.isEmpty || !_wf.hasNode(from)) return false;
    if (name == from) return true;
    if (_wf.hasNode(name)) return false;
    final conns = _wf.connections.clone()..renameNode(from, name);
    var next = _wf.copyWith(
      nodes: [for (final n in _wf.nodes) n.name == from ? n.withName(name) : n],
      connections: conns,
    );
    if (_wf.pinData.containsKey(from)) {
      next = next.withField('pinData', {
        for (final e in _wf.pinData.entries)
          (e.key == from ? name : e.key): e.value,
      });
    }
    _commit(next);
    if (_selection.remove(from)) _selection.add(name);
    notifyListeners();
    return true;
  }

  /// Apply [change] to one node's raw JSON (parameters, notes, disabled, ...).
  /// Edits sharing a [coalesce] key within 1.5s merge into one undo step.
  void updateNode(
    String name,
    FlowNode Function(FlowNode) change, {
    String? coalesce,
  }) {
    final cur = _wf.node(name);
    if (cur == null) return;
    final next = change(cur);
    if (identical(next, cur)) return;
    final wf = _wf.copyWith(
      nodes: [for (final n in _wf.nodes) n.name == name ? next : n],
    );
    final key = coalesce == null ? null : '$name|$coalesce';
    final now = DateTime.now();
    if (key != null &&
        key == _coalesceKey &&
        _undo.isNotEmpty &&
        now.difference(_coalesceAt) < const Duration(milliseconds: 1500)) {
      _wf = wf;
      _coalesceAt = now;
      notifyListeners();
      return;
    }
    _commit(wf);
    _coalesceKey = key;
    _coalesceAt = now;
  }

  void setParameters(
    String name,
    Map<String, dynamic> params, {
    String? coalesce,
  }) => updateNode(name, (n) => n.withParameters(params), coalesce: coalesce);

  void setDisabled(String name, bool disabled) => updateNode(
    name,
    (n) => disabled ? n.set('disabled', true) : n.remove('disabled'),
  );

  void setNotes(String name, String notes) => updateNode(
    name,
    (n) => notes.isEmpty ? n.remove('notes') : n.set('notes', notes),
  );

  void renameWorkflow(String name) {
    if (name == _wf.name) return;
    _commit(_wf.withName(name));
  }

  // connections

  /// False for self-loops, unknown nodes, duplicates and mismatched types.
  bool connect(FlowEdge e) {
    if (e.src == e.dst || !_wf.hasNode(e.src) || !_wf.hasNode(e.dst)) {
      return false;
    }
    if (_wf.connections.contains(e)) return false;
    final conns = _wf.connections.clone()..add(e);
    _commit(_wf.copyWith(connections: conns));
    return true;
  }

  bool disconnect(FlowEdge e) {
    if (!_wf.connections.contains(e)) return false;
    final conns = _wf.connections.clone()..remove(e);
    _commit(_wf.copyWith(connections: conns));
    return true;
  }

  // clipboard

  /// n8n clipboard JSON for the current selection, or null when empty.
  String? copySelection() =>
      _selection.isEmpty ? null : exportNodes(_wf, _selection);

  /// Cut = copy + delete. Returns the JSON.
  String? cutSelection() {
    final s = copySelection();
    if (s != null) deleteNodes(_selection.toList());
    return s;
  }

  /// Paste n8n nodes. Returns the pasted names; empty when [text] is not n8n
  /// JSON. Throws [FlowFormatException] for malformed n8n JSON.
  List<String> paste(String text, {Pos? at}) {
    final payload = parseClipboard(text);
    if (payload == null) return const [];
    final r = pasteInto(_wf, payload, at: at);
    _commit(r.workflow);
    _selection
      ..clear()
      ..addAll(r.names);
    _selectedEdge = null;
    notifyListeners();
    return r.names;
  }

  /// Replace the whole document from n8n workflow JSON, as one undo step.
  /// Keeps the current name when the import has none.
  void importWorkflow(String text) {
    var next = FlowWorkflow.parse(text);
    if (next.name.isEmpty && _wf.name.isNotEmpty) {
      next = next.withName(_wf.name);
    }
    _commit(next);
    _selection.clear();
    _selectedEdge = null;
    notifyListeners();
  }

  String exportWorkflow() => _wf.toJsonString();
}
