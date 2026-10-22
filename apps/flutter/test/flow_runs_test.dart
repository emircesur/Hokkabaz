import 'dart:async';
import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/flows/items_view.dart';
import 'package:hokkabaz/screens/flows/model/flow_editor.dart';
import 'package:hokkabaz/screens/flows/model/run_overlay.dart';
import 'package:hokkabaz/screens/flows/model/workflow_model.dart';
import 'package:hokkabaz/screens/flows/node_io_tab.dart';
import 'package:hokkabaz/screens/flows/run_controller.dart';
import 'package:hokkabaz/screens/runs/run_format.dart';

FlowWorkflow flow() => FlowWorkflow.fromJson({
  'name': 'T',
  'nodes': [
    for (final n in ['Trigger', 'If', 'Yes', 'No'])
      {
        'id': n,
        'name': n,
        'type': 'n8n-nodes-base.noOp',
        'typeVersion': 1,
        'position': [0, 0],
        'parameters': {},
      },
  ],
  'connections': {
    'Trigger': {
      'main': [
        [
          {'node': 'If', 'type': 'main', 'index': 0},
        ],
      ],
    },
    'If': {
      'main': [
        [
          {'node': 'Yes', 'type': 'main', 'index': 0},
        ],
        [
          {'node': 'No', 'type': 'main', 'index': 0},
        ],
      ],
    },
  },
});

String mainJson(List<List<Map<String, dynamic>>?> slots) => jsonEncode(slots);

RunEvent finished(
  String node,
  List<List<Map<String, dynamic>>?> slots, {
  int run = 0,
  String status = 'success',
  int ms = 5,
  String? error,
}) => RunEvent(
  kind: RunEventKind.nodeFinished,
  node: node,
  runIndex: run,
  status: status,
  itemCount: slots.fold(0, (a, s) => a + (s?.length ?? 0)),
  durationMs: ms,
  outputJson: mainJson(slots),
  error: error,
);

Map<String, dynamic> item(Map<String, dynamic> j) => {'json': j};

void main() {
  group('RunOverlay', () {
    test('folds events into per-node status, counts and duration', () {
      var o = const RunOverlay(runId: 'r1');
      o = o.apply(const RunEvent(kind: RunEventKind.runStarted, startedMs: 10));
      o = o.apply(
        const RunEvent(kind: RunEventKind.nodeStarted, node: 'Trigger'),
      );
      expect(o.of('Trigger')!.status, NodeStatus.running);
      o = o.apply(finished('Trigger', [
        [item({'a': 1})],
      ]));
      o = o.apply(finished('If', [
        [item({'a': 1}), item({'a': 2})],
        null,
      ], ms: 12));
      final t = o.of('Trigger')!;
      expect(t.status, NodeStatus.success);
      expect(t.itemCount, 1);
      expect(o.of('If')!.itemCount, 2);
      expect(o.of('If')!.durationMs, 12);
      expect(o.of('Nope'), isNull);
      expect(o.running, isTrue);
      o = o.apply(
        const RunEvent(
          kind: RunEventKind.runFinished,
          status: 'success',
          durationMs: 40,
        ),
      );
      expect(o.state, RunState.success);
      expect(o.running, isFalse);
      expect(o.durationMs, 40);
    });

    test('loops keep every run; an error anywhere marks the node failed', () {
      var o = const RunOverlay(runId: 'r');
      o = o.apply(finished('Loop', [
        [item({'i': 0})],
      ], ms: 3));
      o = o.apply(finished('Loop', [
        [item({'i': 1})],
      ], run: 1, status: 'error', ms: 4, error: 'boom'));
      final n = o.of('Loop')!;
      expect(n.runs, hasLength(2));
      expect(n.status, NodeStatus.error);
      expect(n.error, 'boom');
      expect(n.durationMs, 7);
      expect(n.itemCount, 1, reason: 'badge shows the latest run');
    });

    test('run error keeps the message and the failing node', () {
      final o = const RunOverlay(runId: 'r').apply(
        const RunEvent(
          kind: RunEventKind.runError,
          node: 'If',
          error: 'bad expression',
          durationMs: 9,
        ),
      );
      expect(o.state, RunState.error);
      expect(o.error, 'bad expression');
      expect(o.errorNode, 'If');
    });

    test('input of a node is what connected upstream outputs emitted', () {
      var o = const RunOverlay(runId: 'r');
      o = o.apply(finished('If', [
        [item({'v': 'yes'})],
        [item({'v': 'no1'}), item({'v': 'no2'})],
      ]));
      final wf = flow();
      expect(o.inputItems(wf, 'Yes').map((i) => (i['json'] as Map)['v']), [
        'yes',
      ]);
      expect(o.inputItems(wf, 'No'), hasLength(2));
      expect(o.inputSources(wf, 'No'), ['If']);
      expect(o.inputItems(wf, 'Trigger'), isEmpty);
    });

    test('stored run data builds the same overlay, in execution order', () {
      final data = jsonEncode({
        'If': [
          {
            'startTime': 2,
            'executionTime': 7,
            'executionIndex': 1,
            'executionStatus': 'success',
            'itemCounts': [120, 0],
            'truncated': true,
            'data': {
              'main': [
                [
                  {'json': {'a': 1}},
                ],
                null,
              ],
            },
          },
        ],
        'Trigger': [
          {
            'startTime': 1,
            'executionTime': 1,
            'executionIndex': 0,
            'executionStatus': 'success',
            'data': {
              'main': [
                [
                  {'json': {}},
                ],
              ],
            },
          },
        ],
        'Yes': [
          {
            'executionIndex': 2,
            'executionStatus': 'error',
            'error': {'message': 'nope'},
            'data': {'main': []},
          },
        ],
      });
      final o = RunOverlay.fromRunData('r', data, status: 'error');
      expect(o.nodes.keys, ['Trigger', 'If', 'Yes']);
      expect(o.of('If')!.itemCount, 120, reason: 'true count, not the cut list');
      expect(o.of('If')!.last.output(0), hasLength(1));
      expect(o.of('If')!.last.truncated, isTrue);
      expect(o.of('Yes')!.error, 'nope');
      expect(o.errorNode, 'Yes');
      expect(o.state, RunState.error);
    });

    test('garbage run data yields an empty overlay', () {
      expect(RunOverlay.fromRunData('r', 'not json').nodes, isEmpty);
    });
  });

  group('pin data', () {
    test('pin stores {json} items under workflow.pinData and unpin removes', () {
      final ed = FlowEditor(flow());
      ed.pinItems('If', toPinItems([
        {
          'json': {'a': 1},
          'binary': {'f': {}},
          'pairedItem': {'item': 0},
        },
      ]));
      expect(ed.isPinned('If'), isTrue);
      expect(ed.workflow.pinData['If'], [
        {
          'json': {'a': 1},
        },
      ]);
      expect(ed.dirty, isTrue);
      ed.undo();
      expect(ed.isPinned('If'), isFalse);
      ed.redo();
      ed.unpin('If');
      expect(ed.workflow.field('pinData'), isNull);
      ed.pinItems('Ghost', const []);
      expect(ed.workflow.pinData, isEmpty, reason: 'unknown node ignored');
    });

    test('pinned items survive a JSON round trip and renames', () {
      final ed = FlowEditor(flow())
        ..pinItems('Yes', [
          {
            'json': {'k': 'v'},
          },
        ]);
      ed.renameNode('Yes', 'Accepted');
      final back = FlowWorkflow.parse(ed.workflow.toJsonString());
      expect(back.pinData.keys, ['Accepted']);
      expect(back.validate(), isEmpty);
    });

    test('pinned items read both n8n shapes', () {
      final items = pinnedItems([
        {
          'json': {'a': 1},
        },
        {'b': 2},
      ]);
      expect(items[0]['json'], {'a': 1});
      expect(items[1]['json'], {'b': 2});
      expect(pinnedItems('nope'), isEmpty);
    });

    test('active flag is mirrored into the document', () {
      final ed = FlowEditor(flow())..setActive(true);
      expect(ed.workflow.active, isTrue);
      ed.setActive(true);
      expect(ed.canUndo, isTrue);
    });
  });

  group('FlowRunController', () {
    test('starts a run and folds streamed events; id and trigger go through', () async {
      final feed = StreamController<RunEvent>();
      String? sentJson;
      String? sentTrigger;
      bool? sentTest;
      final c = FlowRunController(
        start: ({required workflowJson, required test, triggerNode}) async {
          sentJson = workflowJson;
          sentTest = test;
          sentTrigger = triggerNode;
          return 'run-1';
        },
        events: (id) => feed.stream,
      );
      final seen = <RunState?>[];
      c.addListener(() => seen.add(c.overlay?.state));

      await c.run(flow(), flowId: 'flow_x', triggerNode: 'Trigger');
      expect(jsonDecode(sentJson!)['id'], 'flow_x');
      expect(sentTest, isTrue);
      expect(sentTrigger, 'Trigger');
      expect(c.running, isTrue);
      expect(c.overlay!.runId, 'run-1');

      feed.add(finished('Trigger', [
        [item({})],
      ]));
      feed.add(
        const RunEvent(kind: RunEventKind.runFinished, status: 'success'),
      );
      await Future<void>.delayed(Duration.zero);
      expect(c.overlay!.of('Trigger')!.itemCount, 1);
      expect(c.running, isFalse);
      await feed.close();
      c.dispose();
    });

    test('a failed start is an error, not an overlay', () async {
      final c = FlowRunController(
        start: ({required workflowJson, required test, triggerNode}) async =>
            throw StateError('no trigger'),
        events: (_) => const Stream.empty(),
      );
      await c.run(flow(), flowId: 'f');
      expect(c.overlay, isNull);
      expect(c.error, contains('no trigger'));
      expect(c.running, isFalse);
      c.dispose();
    });

    test('a second run is refused while one is running; show/clear replace it', () async {
      var starts = 0;
      final c = FlowRunController(
        start: ({required workflowJson, required test, triggerNode}) async =>
            'run-${++starts}',
        events: (_) => const Stream.empty(),
      );
      await c.run(flow(), flowId: 'f');
      await c.run(flow(), flowId: 'f');
      expect(starts, 1);
      c.show(RunOverlay.fromRunData('old', '{}'));
      expect(c.overlay!.runId, 'old');
      expect(c.running, isFalse);
      c.clear();
      expect(c.overlay, isNull);
      c.dispose();
    });
  });

  group('formatting', () {
    test('durations and item labels', () {
      expect(formatDuration(850), '850 ms');
      expect(formatDuration(1234), '1.2 s');
      expect(formatDuration(12400), '12 s');
      expect(formatDuration(125000), '2 m 5 s');
      expect(itemsLabel(1), '1 item');
      expect(itemsLabel(0), '0 items');
    });

    test('relative time and the local flow id of a run', () {
      const now = 1000000000;
      expect(relativeTime(now - 1000, nowMs: now), 'just now');
      expect(relativeTime(now - 5 * 60000, nowMs: now), '5 min ago');
      expect(relativeTime(now - 3 * 3600000, nowMs: now), '3 h ago');
      expect(relativeTime(now - 2 * 86400000, nowMs: now), '2 d ago');
      expect(localFlowIdOf('{"id":"flow_a"}'), 'flow_a');
      expect(
        localFlowIdOf('{"id":"remote7","meta":{"hokkabaz":{"localId":"flow_a"}}}'),
        'flow_a',
      );
      expect(localFlowIdOf('nope'), isNull);
      expect(localFlowIdOf(null), isNull);
    });

    test('table columns are the union of json keys; cells are bounded', () {
      final cols = itemColumns([
        item({'a': 1, 'b': 2}),
        item({'b': 3, 'c': 4}),
      ]);
      expect(cols, ['a', 'b', 'c']);
      expect(cellText(null), '');
      expect(cellText({'x': 1}), '{"x":1}');
      expect(cellText('y' * 200), endsWith('...'));
      expect(cellText('y' * 200).length, lessThan(100));
    });
  });
}
