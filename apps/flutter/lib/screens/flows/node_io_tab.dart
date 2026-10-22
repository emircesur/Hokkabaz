import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'items_view.dart';
import 'model/flow_editor.dart';
import 'model/run_overlay.dart';
import 'model/workflow_model.dart';

enum IoSide { input, output }

/// Items of a pinData entry as run items: n8n accepts `{json: {..}}` and bare objects.
List<FlowItem> pinnedItems(Object? raw) {
  if (raw is! List) return const [];
  return [
    for (final it in raw)
      if (it is Map)
        it['json'] is Map
            ? Map<String, dynamic>.from(it)
            : {'json': Map<String, dynamic>.from(it)},
  ];
}

/// Input / Output tab of the node panel: what the node received / emitted in the run
/// shown on the canvas, plus the pin toggle on Output.
class NodeIoTab extends StatefulWidget {
  const NodeIoTab({
    super.key,
    required this.editor,
    required this.nodeName,
    required this.overlay,
    required this.side,
  });

  final FlowEditor editor;
  final String nodeName;
  final RunOverlay? overlay;
  final IoSide side;

  @override
  State<NodeIoTab> createState() => _NodeIoTabState();
}

class _NodeIoTabState extends State<NodeIoTab> {
  int? _run;
  int _out = 0;

  @override
  Widget build(BuildContext context) {
    final wf = widget.editor.workflow;
    return widget.side == IoSide.input
        ? _input(context, wf)
        : _output(context, wf);
  }

  Widget _hint(BuildContext context, String msg) => Padding(
    padding: const EdgeInsets.all(HkTokens.s4),
    child: Text(
      msg,
      style: Theme.of(context).textTheme.bodySmall
          ?.copyWith(color: context.hk.textMuted),
    ),
  );

  Widget _input(BuildContext context, FlowWorkflow wf) {
    final o = widget.overlay;
    if (o == null) {
      return _hint(
        context,
        'Run the workflow (Test workflow) to see the input of this node.',
      );
    }
    final from = o.inputSources(wf, widget.nodeName);
    final items = o.inputItems(wf, widget.nodeName);
    if (from.isEmpty) {
      return _hint(
        context,
        o.of(widget.nodeName) == null
            ? 'This node did not run.'
            : 'Nothing feeds this node (it starts the flow).',
      );
    }
    // Counts come from upstream's true counts, the list from what was kept.
    var total = 0;
    for (final e in wf.connections.edges) {
      if (e.dst != widget.nodeName || e.type != 'main') continue;
      final up = o.of(e.src)?.last;
      if (up == null) continue;
      total += e.srcIndex < up.counts.length ? up.counts[e.srcIndex] : 0;
    }
    return ListView(
      children: [
        Padding(
          padding: const EdgeInsets.fromLTRB(
            HkTokens.s3,
            HkTokens.s3,
            HkTokens.s3,
            0,
          ),
          child: Text(
            'From ${from.join(', ')}',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
        ItemsView(
          items: items,
          total: total,
          empty: 'No items reached this node',
        ),
      ],
    );
  }

  Widget _output(BuildContext context, FlowWorkflow wf) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final name = widget.nodeName;
    final node = widget.overlay?.of(name);
    final pinned = widget.editor.isPinned(name);
    final run = node == null
        ? null
        : node.runs[(_run ?? node.runs.length - 1).clamp(
            0,
            node.runs.length - 1,
          )];
    final main = run?.main ?? const <List<FlowItem>?>[];
    final out = _out.clamp(0, main.isEmpty ? 0 : main.length - 1);
    final canPin = pinned || (run != null && run.output(0).isNotEmpty);

    final children = <Widget>[
      SwitchListTile(
        // A switch is a 48dp target by itself; the tile keeps the whole row tappable.
        contentPadding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
        title: const Text('Pin data'),
        subtitle: Text(
          pinned
              ? 'Test runs use this output instead of executing the node.'
              : canPin
              ? 'Freeze the first output so test runs skip this node.'
              : 'Run the node first, then pin its output.',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
        value: pinned,
        onChanged: canPin
            ? (v) {
                if (!v) {
                  widget.editor.unpin(name);
                } else if (run != null) {
                  widget.editor.pinItems(name, toPinItems(run.output(0)));
                }
              }
            : null,
      ),
      if (pinned && run != null && run.truncated)
        _note(
          context,
          'The run output was cut; only the shown items are pinned.',
        ),
    ];

    if (pinned) {
      final items = pinnedItems(wf.pinData[name]);
      children
        ..add(_label(context, 'Pinned data'))
        ..add(ItemsView(items: items, empty: 'Pinned data is empty'));
    } else if (node == null) {
      children.add(
        _hint(
          context,
          widget.overlay == null
              ? 'Run the workflow (Test workflow) to see the output of this node.'
              : 'This node did not run.',
        ),
      );
    } else {
      if (node.runs.length > 1) {
        children.add(_runPicker(context, node, _run ?? node.runs.length - 1));
      }
      children.add(_status(context, run!));
      if (main.length > 1) children.add(_outputPicker(context, run, out));
      children.add(
        ItemsView(
          key: ValueKey('$name/${_run ?? -1}/$out'),
          items: run.output(out),
          total: out < run.counts.length ? run.counts[out] : null,
          empty: run.status == NodeStatus.error
              ? 'The node failed'
              : 'No items on this output',
        ),
      );
    }
    return ListView(children: children);
  }

  Widget _label(BuildContext context, String t) => Padding(
    padding: const EdgeInsets.fromLTRB(
      HkTokens.s3,
      HkTokens.s2,
      HkTokens.s3,
      0,
    ),
    child: Text(t, style: Theme.of(context).textTheme.labelMedium),
  );

  Widget _note(BuildContext context, String t) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
    child: Text(
      t,
      style: Theme.of(context).textTheme.bodySmall
          ?.copyWith(color: context.hk.warn),
    ),
  );

  Widget _status(BuildContext context, NodeRunInfo run) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final (label, color) = switch (run.status) {
      NodeStatus.success => ('Success', hk.ok),
      NodeStatus.error => ('Error', hk.err),
      NodeStatus.waiting => ('Waiting', hk.warn),
      NodeStatus.running => ('Running', hk.info),
    };
    return Padding(
      padding: const EdgeInsets.all(HkTokens.s3),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text.rich(
            TextSpan(
              children: [
                TextSpan(
                  text: label,
                  style: TextStyle(color: color, fontWeight: FontWeight.w600),
                ),
                TextSpan(
                  text:
                      ' · ${itemsLabel(run.itemCount)} · ${formatDuration(run.durationMs)}',
                ),
              ],
            ),
            style: text.bodySmall,
          ),
          if (run.error != null)
            Container(
              width: double.infinity,
              margin: const EdgeInsets.only(top: HkTokens.s2),
              padding: const EdgeInsets.all(HkTokens.s2),
              decoration: BoxDecoration(
                color: hk.err.withValues(alpha: 0.12),
                borderRadius: BorderRadius.circular(HkTokens.radiusCard),
                border: Border.all(color: hk.err),
              ),
              child: SelectableText(run.error!, style: text.bodySmall),
            ),
        ],
      ),
    );
  }

  Widget _runPicker(BuildContext context, NodeOverlay node, int cur) => Padding(
    padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
    child: Row(
      children: [
        IconButton(
          tooltip: 'Previous run',
          icon: const Icon(Icons.chevron_left),
          onPressed: cur > 0 ? () => setState(() => _run = cur - 1) : null,
        ),
        Text('Run ${cur + 1} of ${node.runs.length}'),
        IconButton(
          tooltip: 'Next run',
          icon: const Icon(Icons.chevron_right),
          onPressed: cur < node.runs.length - 1
              ? () => setState(() => _run = cur + 1)
              : null,
        ),
      ],
    ),
  );

  Widget _outputPicker(BuildContext context, NodeRunInfo run, int cur) =>
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
        child: Wrap(
          spacing: HkTokens.s2,
          children: [
            for (var i = 0; i < run.main.length; i++)
              ChoiceChip(
                materialTapTargetSize: MaterialTapTargetSize.padded,
                label: Text(
                  'Output $i (${i < run.counts.length ? run.counts[i] : 0})',
                ),
                selected: i == cur,
                onSelected: (_) => setState(() => _out = i),
              ),
          ],
        ),
      );
}
