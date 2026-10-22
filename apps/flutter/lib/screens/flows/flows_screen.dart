import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../src/rust/api/flow_runs.dart' as run_api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import '../runs/runs_providers.dart';
import 'canvas_geometry.dart';
import 'flow_canvas.dart';
import 'flow_providers.dart';
import 'flow_store.dart';
import 'model/flow_editor.dart';
import 'model/node_types.dart';
import 'model/workflow_model.dart';
import 'node_panel.dart';
import 'node_picker.dart';
import 'run_bridge.dart';
import 'run_controller.dart';

/// DESIGN §4.4: flows list | canvas | node panel. Workflows are n8n JSON files in
/// the app data dir for now; hk-store / REST persistence replaces [FlowStore] later.
class FlowsScreen extends ConsumerStatefulWidget {
  const FlowsScreen({super.key});

  @override
  ConsumerState<FlowsScreen> createState() => _FlowsScreenState();
}

class _FlowsScreenState extends ConsumerState<FlowsScreen> {
  final _editor = FlowEditor();
  final _canvasKey = GlobalKey<FlowCanvasState>();
  final FlowRunController _runs = ffiRunController();
  FlowStore? _store;
  List<FlowFile> _files = const [];
  String? _currentId;
  String? _error;
  bool _loading = true;
  bool _panelOpen = true;
  Timer? _autosave;
  bool _saving = false;
  String? _savedName;

  @override
  void initState() {
    super.initState();
    _editor.addListener(_onEdit);
    _runs.addListener(_onRunChange);
    _init();
  }

  void _onRunChange() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _autosave?.cancel();
    _editor.removeListener(_onEdit);
    _runs.removeListener(_onRunChange);
    _runs.dispose();
    // Fire-and-forget: the store outlives the widget and a dirty doc must not be lost.
    if (_editor.dirty && _currentId != null) unawaited(_save());
    _editor.dispose();
    super.dispose();
  }

  Future<void> _init() async {
    try {
      _store = await FlowStore.open();
      await _refresh();
      final pending = ref.read(pendingFlowOpenProvider);
      if (pending != null) {
        ref.read(pendingFlowOpenProvider.notifier).set(null);
        await _openRun(pending);
      } else if (_files.isNotEmpty) {
        await _open(_files.first.id);
      }
    } catch (e) {
      _error = e.toString();
    }
    if (mounted) setState(() => _loading = false);
  }

  Future<void> _refresh() async {
    final s = _store;
    if (s == null) return;
    final l = await s.list();
    if (mounted) setState(() => _files = l);
  }

  void _onEdit() {
    if (!mounted) return;
    setState(() {});
    if (_editor.dirty && _currentId != null) {
      _autosave?.cancel();
      _autosave = Timer(const Duration(milliseconds: 800), _save);
    }
  }

  Future<void> _save() async {
    final s = _store;
    final id = _currentId;
    if (s == null || id == null || _saving) return;
    _saving = true;
    final wf = _editor.workflow;
    try {
      await s.save(id, wf);
      if (mounted && identical(_editor.workflow, wf)) _editor.markSaved();
      if (_savedName != wf.name) {
        _savedName = wf.name;
        await _refresh();
      }
    } catch (e) {
      _toast('Save failed: $e');
    } finally {
      _saving = false;
    }
    // Edits that landed during the write.
    if (mounted && _editor.dirty) _onEdit();
  }

  Future<void> _open(String id) async {
    final s = _store;
    if (s == null) return;
    if (_editor.dirty) await _save();
    try {
      final wf = await s.load(id);
      _runs.clear();
      _editor.load(wf);
      _savedName = wf.name;
      if (!mounted) return;
      setState(() {
        _currentId = id;
        _error = null;
      });
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => _canvasKey.currentState?.fit(),
      );
    } on FlowFormatException catch (e) {
      _toast('Cannot open flow: ${e.message}');
    } catch (e) {
      _toast('Cannot open flow: $e');
    }
  }

  /// Show a stored run (from the Runs screen): on the flow it ran when that is
  /// on this device, else on a copy of the workflow snapshot.
  Future<void> _openRun(PendingFlowOpen p) async {
    final s = _store;
    if (s == null) return;
    var id = p.flowId;
    if (id != null && !_files.any((f) => f.id == id)) id = null;
    try {
      if (id == null) {
        final json = p.workflowJson;
        if (json == null) return _toast('This run has no workflow snapshot');
        final wf = FlowWorkflow.parse(json)
            .withoutField('id')
            .withoutField('active');
        final f = await s.create(
          '${wf.name.isEmpty ? 'Workflow' : wf.name} (run copy)',
          from: wf,
        );
        await _refresh();
        id = f.id;
      }
      await _open(id);
      _runs.show(p.overlay);
    } catch (e) {
      _toast('Cannot open run: ${describeError(e)}');
    }
  }

  /// Test (pinned data honoured) or live-once run from the selected trigger, else
  /// the Manual Trigger.
  Future<void> _runFlow({required bool test}) async {
    final id = _currentId;
    if (id == null || _runs.running) return;
    if (_editor.dirty) await _save();
    final wf = _editor.workflow;
    final errors = <String>[
      ...wf.validate(),
      ...validateWithEngine(_editor.exportWorkflow())
          .where((i) => i.startsWith('error'))
          .map((i) => i.replaceFirst('error: ', '')),
    ];
    if (errors.isNotEmpty) return _toast('Cannot run: ${errors.first}');
    final types = ref.read(nodeTypesProvider).byType;
    final sel = _editor.selectedNode;
    String? trigger;
    if (sel != null &&
        categoryOf(sel.type, group: types[sel.type]?.group ?? const []) ==
            NodeCategory.trigger) {
      trigger = sel.name;
    }
    await _runs.run(wf, flowId: id, test: test, triggerNode: trigger);
    if (_runs.error != null) _toast(_runs.error!);
  }

  Future<void> _toggleActive(bool value) async {
    final id = _currentId;
    if (id == null) return;
    try {
      if (_editor.dirty) await _save();
      final json = _editor.workflow
          .withField('id', id)
          .toJsonString(pretty: false);
      final now = await run_api.setWorkflowActive(
        workflowJson: json,
        active: value,
      );
      _editor.setActive(now);
      _toast(now ? 'Workflow activated on the instance' : 'Workflow deactivated');
    } catch (e) {
      _toast('Activation failed: ${describeError(e)}');
    }
  }

  void _toast(String m) {
    if (!mounted) return;
    ScaffoldMessenger.maybeOf(context)
      ?..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
  }

  // flow list ops

  Future<String?> _askName(String title, String initial) => showDialog<String>(
    context: context,
    builder: (ctx) {
      final c = TextEditingController(text: initial);
      return AlertDialog(
        title: Text(title),
        content: TextField(
          controller: c,
          autofocus: true,
          decoration: const InputDecoration(labelText: 'Name'),
          onSubmitted: (v) => Navigator.pop(ctx, v.trim()),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, c.text.trim()),
            child: const Text('OK'),
          ),
        ],
      );
    },
  );

  Future<void> _create({FlowWorkflow? from, String? suggested}) async {
    final s = _store;
    if (s == null) return;
    final name = await _askName('New flow', suggested ?? 'My workflow');
    if (name == null || name.isEmpty) return;
    final f = await s.create(name, from: from);
    await _refresh();
    await _open(f.id);
  }

  Future<void> _rename(FlowFile f) async {
    final name = await _askName('Rename flow', f.name);
    if (name == null || name.isEmpty || name == f.name) return;
    if (f.id == _currentId) {
      _editor.renameWorkflow(name);
      await _save();
    } else {
      await _store?.rename(f.id, name);
      await _refresh();
    }
  }

  Future<void> _delete(FlowFile f) async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('Delete "${f.name}"?'),
        content: const Text('The workflow file is removed from this device.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx, false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(ctx, true),
            child: const Text('Delete'),
          ),
        ],
      ),
    );
    if (ok != true) return;
    if (f.id == _currentId) {
      _autosave?.cancel();
      _editor.load(FlowWorkflow.empty());
      setState(() => _currentId = null);
    }
    await _store?.delete(f.id);
    await _refresh();
    if (_currentId == null && _files.isNotEmpty) await _open(_files.first.id);
  }

  // import / export

  Future<void> _importText(String text) async {
    try {
      final wf = FlowWorkflow.parse(text);
      await _create(
        from: wf,
        suggested: wf.name.isEmpty ? 'Imported workflow' : wf.name,
      );
    } on FlowFormatException catch (e) {
      _toast('Import failed: ${e.message}');
    }
  }

  Future<void> _importFromClipboard() async {
    final t = (await Clipboard.getData(Clipboard.kTextPlain))?.text;
    if (t == null || t.trim().isEmpty) return _toast('Clipboard is empty');
    await _importText(t);
  }

  Future<void> _jsonDialog({required bool export}) async {
    final c = TextEditingController(
      text: export ? _editor.exportWorkflow() : '',
    );
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text(
          export ? 'Export n8n workflow JSON' : 'Import n8n workflow JSON',
        ),
        content: SizedBox(
          width: 560,
          child: TextField(
            controller: c,
            readOnly: export,
            autofocus: !export,
            minLines: 12,
            maxLines: 18,
            style: const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 12),
            decoration: InputDecoration(
              hintText: export ? null : 'Paste a workflow exported from n8n',
              border: const OutlineInputBorder(),
            ),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
          if (export)
            FilledButton(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: c.text));
                if (ctx.mounted) Navigator.pop(ctx);
                _toast('Workflow JSON copied');
              },
              child: const Text('Copy'),
            )
          else
            FilledButton(
              onPressed: () {
                Navigator.pop(ctx);
                unawaited(_importText(c.text));
              },
              child: const Text('Import as new flow'),
            ),
        ],
      ),
    );
    c.dispose();
  }

  Future<void> _validate() async {
    final issues = <String>{
      ..._editor.workflow.validate().map((e) => 'error: $e'),
      ...validateWithEngine(_editor.exportWorkflow()),
    };
    if (!mounted) return;
    if (issues.isEmpty) return _toast('No issues found');
    await showDialog<void>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: const Text('Validation'),
        content: SizedBox(
          width: 480,
          child: ListView(
            shrinkWrap: true,
            children: [
              for (final i in issues)
                ListTile(
                  dense: true,
                  leading: Icon(
                    i.startsWith('warning')
                        ? Icons.warning_amber_rounded
                        : Icons.error_outline,
                    color: i.startsWith('warning') ? ctx.hk.warn : ctx.hk.err,
                  ),
                  title: Text(
                    i.replaceFirst(RegExp(r'^(warning|error): '), ''),
                  ),
                ),
            ],
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(ctx),
            child: const Text('Close'),
          ),
        ],
      ),
    );
  }

  // canvas actions

  Future<void> _addNode(Pos? at, PortRef? from) async {
    final types = ref.read(nodeTypesProvider);
    final pick = await showNodePicker(context, types.list);
    if (pick == null || !mounted) return;
    final sel = _editor.selectedNode;
    String? connectFrom;
    var fromIndex = 0;
    Pos pos;
    if (from != null) {
      connectFrom = from.node;
      fromIndex = from.index;
      pos = at ?? (x: 0, y: 0);
    } else if (at == null && sel != null) {
      // Toolbar/Tab with one node selected: chain to its right (n8n behaviour).
      connectFrom = sel.name;
      pos = (x: sel.position.x + kNodeWidth + 60, y: sel.position.y);
    } else {
      final c = at ?? _canvasKey.currentState?.viewCenter ?? (x: 0.0, y: 0.0);
      pos = at ?? (x: c.x - kNodeWidth / 2, y: c.y - kNodeMinHeight / 2);
    }
    final info = pick.info;
    if (info != null) {
      _editor.addNode(
        info,
        pos,
        connectFrom: connectFrom,
        connectFromIndex: fromIndex,
      );
    } else {
      final t = pick.customType!;
      _editor.addRawNode(
        type: t,
        version: 1,
        baseName: shortType(t),
        at: pos,
        connectFrom: connectFrom,
        connectFromIndex: fromIndex,
      );
    }
  }

  Future<void> _openNodeSheet(String name) async {
    final types = ref.read(nodeTypesProvider).byType;
    await showModalBottomSheet<void>(
      context: context,
      isScrollControlled: true,
      useSafeArea: true,
      builder: (ctx) {
        // Sheet context: keeps the keyboard inset live so params stay visible.
        final inset = MediaQuery.viewInsetsOf(ctx).bottom;
        final h = MediaQuery.sizeOf(ctx).height;
        return Padding(
          padding: EdgeInsets.only(bottom: inset),
          child: SizedBox(
            height: (h * 0.85 - inset).clamp(240, h),
            child: NodePanel(
              editor: _editor,
              nodeName: name,
              types: types,
              onClose: () => Navigator.pop(ctx),
              overlay: _runs.overlay,
            ),
          ),
        );
      },
    );
  }

  // build

  @override
  Widget build(BuildContext context) {
    if (_loading) return const Center(child: CircularProgressIndicator());
    final types = ref.watch(nodeTypesProvider);
    return LayoutBuilder(
      builder: (context, c) {
        final wide = c.maxWidth >= kFlowsWideMin;
        final sel = _editor.selectedNode;
        final showPanel = wide && _panelOpen && sel != null;
        return Row(
          children: [
            if (wide) ...[
              SizedBox(width: 240, child: _listPane()),
              VerticalDivider(width: 1, color: context.hk.border),
            ],
            Expanded(
              child: Column(
                children: [
                  _toolbar(wide, types),
                  Divider(height: 1, color: context.hk.border),
                  Expanded(child: _canvasArea(types)),
                ],
              ),
            ),
            if (showPanel) ...[
              VerticalDivider(width: 1, color: context.hk.border),
              SizedBox(
                width: 340,
                child: NodePanel(
                  editor: _editor,
                  nodeName: sel.name,
                  types: types.byType,
                  onClose: () => setState(() => _panelOpen = false),
                  overlay: _runs.overlay,
                ),
              ),
            ],
          ],
        );
      },
    );
  }

  Widget _canvasArea(NodeTypesResult types) {
    final hk = context.hk;
    if (_currentId == null) {
      return Center(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            Icon(Icons.account_tree_outlined, size: 40, color: hk.textMuted),
            const SizedBox(height: HkTokens.s3),
            Text(
              _error ?? 'No flow open',
              style: Theme.of(context).textTheme.titleMedium,
            ),
            const SizedBox(height: HkTokens.s3),
            Wrap(
              spacing: HkTokens.s2,
              children: [
                FilledButton.icon(
                  onPressed: _create,
                  icon: const Icon(Icons.add),
                  label: const Text('New flow'),
                ),
                OutlinedButton.icon(
                  onPressed: _importFromClipboard,
                  icon: const Icon(Icons.content_paste),
                  label: const Text('Import from clipboard'),
                ),
              ],
            ),
          ],
        ),
      );
    }
    return Stack(
      children: [
        FlowCanvas(
          key: _canvasKey,
          editor: _editor,
          types: types.byType,
          onAddNode: _addNode,
          onOpenNode: (n) {
            if (MediaQuery.sizeOf(context).width >= kFlowsWideMin) {
              setState(() => _panelOpen = true);
            } else {
              _openNodeSheet(n);
            }
          },
          onMessage: _toast,
          onSave: _save,
          overlay: _runs.overlay,
        ),
        if (_runs.overlay?.error != null) _runErrorBanner(),
        if (types.error != null)
          Positioned(
            top: HkTokens.s2,
            left: HkTokens.s2,
            right: HkTokens.s2,
            child: Material(
              color: hk.warn.withValues(alpha: 0.15),
              borderRadius: BorderRadius.circular(HkTokens.radiusCard),
              child: Padding(
                padding: const EdgeInsets.all(HkTokens.s2),
                child: Text(
                  'Node catalog unavailable (${types.error}). All nodes show as unsupported but stay intact.',
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
            ),
          ),
        if (_editor.workflow.nodes.isEmpty)
          IgnorePointer(
            child: Center(
              child: Text(
                isTouchLayout(context)
                    ? 'Long-press to add a node, or paste n8n nodes from the toolbar'
                    : 'Press Tab or long-press to add a node, or paste n8n nodes (Ctrl+V)',
                style: Theme.of(context).textTheme.bodySmall
                    ?.copyWith(color: hk.textMuted),
              ),
            ),
          ),
      ],
    );
  }

  Widget _runErrorBanner() {
    final hk = context.hk;
    final o = _runs.overlay!;
    return Positioned(
      left: HkTokens.s2,
      right: HkTokens.s2,
      bottom: HkTokens.s2,
      child: Material(
        color: hk.surface,
        shape: RoundedRectangleBorder(
          borderRadius: BorderRadius.circular(HkTokens.radiusCard),
          side: BorderSide(color: hk.err),
        ),
        child: Padding(
          padding: const EdgeInsets.all(HkTokens.s2),
          child: Row(
            children: [
              Icon(Icons.error, size: 18, color: hk.err),
              const SizedBox(width: HkTokens.s2),
              Expanded(
                child: Text(
                  o.errorNode == null ? o.error! : '${o.errorNode}: ${o.error}',
                  maxLines: 3,
                  overflow: TextOverflow.ellipsis,
                  style: Theme.of(context).textTheme.bodySmall,
                ),
              ),
              IconButton(
                tooltip: 'Dismiss run result',
                icon: const Icon(Icons.close, size: 18),
                onPressed: _runs.clear,
              ),
            ],
          ),
        ),
      ),
    );
  }

  Widget _listPane() {
    final hk = context.hk;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.all(HkTokens.s2),
          child: FilledButton.icon(
            onPressed: _create,
            icon: const Icon(Icons.add, size: 18),
            label: const Text('New'),
          ),
        ),
        Expanded(
          child: _files.isEmpty
              ? Center(
                  child: Text(
                    'No flows yet',
                    style: Theme.of(context).textTheme.bodySmall,
                  ),
                )
              : ListView(
                  children: [
                    for (final f in _files)
                      ListTile(
                        dense: true,
                        selected: f.id == _currentId,
                        selectedTileColor: hk.accent.withValues(alpha: 0.1),
                        title: Text(f.name, overflow: TextOverflow.ellipsis),
                        onTap: () => _open(f.id),
                        trailing: PopupMenuButton<String>(
                          tooltip: 'Flow actions',
                          icon: const Icon(Icons.more_vert, size: 18),
                          onSelected: (v) =>
                              v == 'rename' ? _rename(f) : _delete(f),
                          itemBuilder: (_) => const [
                            PopupMenuItem(
                              value: 'rename',
                              child: Text('Rename'),
                            ),
                            PopupMenuItem(
                              value: 'delete',
                              child: Text('Delete'),
                            ),
                          ],
                        ),
                      ),
                  ],
                ),
        ),
      ],
    );
  }

  Widget _toolbar(bool wide, NodeTypesResult types) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final hasFlow = _currentId != null;
    final sel = _editor.selection;
    FlowFile? current;
    for (final f in _files) {
      if (f.id == _currentId) current = f;
    }
    return SizedBox(
      height: 48,
      child: ListView(
        scrollDirection: Axis.horizontal,
        padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
        children: [
          if (!wide)
            PopupMenuButton<String>(
              tooltip: 'Flows',
              icon: const Icon(Icons.folder_open),
              onSelected: (v) => v == '__new' ? _create() : _open(v),
              itemBuilder: (_) => [
                for (final f in _files)
                  PopupMenuItem(value: f.id, child: Text(f.name)),
                const PopupMenuDivider(),
                const PopupMenuItem(value: '__new', child: Text('New flow')),
              ],
            ),
          Center(
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
              child: InkWell(
                onTap: current == null ? null : () => _rename(current!),
                child: Text(
                  hasFlow ? _editor.workflow.name : 'Flows',
                  style: text.titleSmall,
                ),
              ),
            ),
          ),
          if (hasFlow)
            Center(
              child: Tooltip(
                message: _editor.dirty ? 'Unsaved changes (autosave)' : 'Saved',
                child: Icon(
                  Icons.circle,
                  size: 8,
                  color: _editor.dirty ? hk.warn : hk.ok,
                ),
              ),
            ),
          const SizedBox(width: HkTokens.s2),
          _btn(
            Icons.add_box_outlined,
            'Add node (Tab)',
            hasFlow ? () => _addNode(null, null) : null,
          ),
          _btn(Icons.undo, 'Undo', _editor.canUndo ? _editor.undo : null),
          _btn(Icons.redo, 'Redo', _editor.canRedo ? _editor.redo : null),
          _btn(
            Icons.copy,
            'Copy selection as n8n JSON',
            sel.isEmpty ? null : () => _canvasKey.currentState?.copy(),
          ),
          _btn(
            Icons.content_paste,
            'Paste n8n nodes',
            hasFlow
                ? () => _canvasKey.currentState?.pasteFromClipboard()
                : null,
          ),
          _btn(
            Icons.delete_outline,
            'Delete selection',
            (sel.isNotEmpty || _editor.selectedEdge != null)
                ? _editor.deleteSelection
                : null,
          ),
          if (!wide && _editor.selectedNode != null)
            _btn(
              Icons.tune,
              'Edit node',
              () => _openNodeSheet(_editor.selectedNode!.name),
            ),
          if (wide && _editor.selectedNode != null && !_panelOpen)
            _btn(
              Icons.view_sidebar_outlined,
              'Show node panel',
              () => setState(() => _panelOpen = true),
            ),
          _btn(Icons.rule, 'Validate', hasFlow ? _validate : null),
          const SizedBox(width: HkTokens.s1),
          _runButtons(wide, hasFlow),
          PopupMenuButton<String>(
            tooltip: 'Import / export',
            icon: const Icon(Icons.import_export),
            onSelected: (v) {
              switch (v) {
                case 'clip':
                  _importFromClipboard();
                case 'import':
                  _jsonDialog(export: false);
                case 'export':
                  _jsonDialog(export: true);
                case 'copy':
                  Clipboard.setData(
                    ClipboardData(text: _editor.exportWorkflow()),
                  );
                  _toast('Workflow JSON copied');
              }
            },
            itemBuilder: (_) => [
              const PopupMenuItem(
                value: 'clip',
                child: Text('Import from clipboard (new flow)'),
              ),
              const PopupMenuItem(
                value: 'import',
                child: Text('Import JSON...'),
              ),
              if (hasFlow)
                const PopupMenuItem(
                  value: 'copy',
                  child: Text('Copy workflow JSON'),
                ),
              if (hasFlow)
                const PopupMenuItem(
                  value: 'export',
                  child: Text('Export JSON...'),
                ),
            ],
          ),
          if (types.list.isEmpty)
            Center(
              child: Tooltip(
                message: types.error ?? 'No node types',
                child: Icon(
                  Icons.warning_amber_rounded,
                  color: hk.warn,
                  size: 18,
                ),
              ),
            ),
        ],
      ),
    );
  }

  Widget _runButtons(bool wide, bool hasFlow) {
    final running = _runs.running;
    final remote = ref.watch(remoteConnectedProvider);
    final spinner = const SizedBox(
      width: 18,
      height: 18,
      child: CircularProgressIndicator(strokeWidth: 2),
    );
    final canRun = hasFlow && !running;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Center(
          child: Tooltip(
            message: 'Run from the selected trigger, using pinned data',
            child: FilledButton.tonalIcon(
              style: FilledButton.styleFrom(minimumSize: const Size(48, 40)),
              onPressed: canRun ? () => _runFlow(test: true) : null,
              icon: running ? spinner : const Icon(Icons.play_arrow, size: 20),
              label: wide ? const Text('Test workflow') : const Text('Test'),
            ),
          ),
        ),
        PopupMenuButton<String>(
          tooltip: 'More ways to run',
          enabled: canRun,
          icon: const Icon(Icons.arrow_drop_down),
          onSelected: (v) {
            if (v == 'live') _runFlow(test: false);
          },
          itemBuilder: (_) => const [
            PopupMenuItem(
              value: 'live',
              child: Text('Run once, ignore pinned data'),
            ),
          ],
        ),
        if (_runs.overlay != null && !running)
          _btn(Icons.layers_clear_outlined, 'Clear run overlay', _runs.clear),
        Tooltip(
          message: remote
              ? 'Arm the triggers of this workflow on the connected instance'
              : 'Activation needs a connected instance. Add one in Instances; locally you can only test.',
          child: Row(
            mainAxisSize: MainAxisSize.min,
            children: [
              const SizedBox(width: HkTokens.s2),
              Text('Active', style: Theme.of(context).textTheme.bodySmall),
              Switch(
                value: _editor.workflow.active,
                onChanged: remote && hasFlow ? _toggleActive : null,
              ),
            ],
          ),
        ),
      ],
    );
  }

  Widget _btn(IconData icon, String tip, VoidCallback? onPressed) => IconButton(
    tooltip: tip,
    icon: Icon(icon, size: 20),
    onPressed: onPressed,
  );
}
