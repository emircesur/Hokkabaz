import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/flows/flow_store.dart';
import 'package:hokkabaz/screens/flows/model/flow_editor.dart';
import 'package:hokkabaz/screens/flows/model/node_types.dart';
import 'package:hokkabaz/screens/flows/model/workflow_model.dart';

/// n8n export with fields the model does not know: they must survive.
const sample = '''
{
  "name": "Triage",
  "nodes": [
    {
      "parameters": {"path": "hook", "httpMethod": "POST", "options": {"rawBody": true}},
      "id": "11111111-1111-4111-8111-111111111111",
      "name": "Webhook",
      "type": "n8n-nodes-base.webhook",
      "typeVersion": 2,
      "position": [-240, 120],
      "webhookId": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
      "futureNodeField": {"keep": ["me", 1, null]}
    },
    {
      "parameters": {"conditions": {"conditions": [{"leftValue": "={{ \$json.x }}", "rightValue": 1}]}},
      "id": "22222222-2222-4222-8222-222222222222",
      "name": "If",
      "type": "n8n-nodes-base.if",
      "typeVersion": 2.2,
      "position": [0, 120],
      "onError": "continueRegularOutput",
      "credentials": {"httpBasicAuth": {"id": "7", "name": "gitea"}}
    },
    {
      "parameters": {},
      "id": "33333333-3333-4333-8333-333333333333",
      "name": "Notify",
      "type": "n8n-nodes-community.ntfy",
      "typeVersion": 1,
      "position": [260.5, 40],
      "disabled": true,
      "notes": "unsupported on purpose"
    },
    {
      "parameters": {},
      "id": "44444444-4444-4444-8444-444444444444",
      "name": "Done",
      "type": "n8n-nodes-base.noOp",
      "typeVersion": 1,
      "position": [260, 220]
    }
  ],
  "connections": {
    "Webhook": {"main": [[{"node": "If", "type": "main", "index": 0}]]},
    "If": {"main": [
      [{"node": "Notify", "type": "main", "index": 0}],
      [{"node": "Done", "type": "main", "index": 0}]
    ]}
  },
  "pinData": {"Webhook": [{"json": {"a": 1}}]},
  "active": false,
  "settings": {"executionOrder": "v1", "timezone": "Europe/Istanbul"},
  "staticData": null,
  "meta": {"templateCredsSetupCompleted": true, "hokkabaz": {"zoom": 0.8}},
  "tags": [{"id": "t1", "name": "ops"}],
  "versionId": "v-1"
}
''';

String canon(Object? v) => jsonEncode(v);

NodeTypeInfo ifInfo() => NodeTypeInfo.fromFields(
  typeName: 'n8n-nodes-base.if',
  versions: [1, 2, 2.2],
  defaultVersion: 2.2,
  displayName: 'If',
  description: 'Route items',
  group: ['transform'],
  inputs: ['main'],
  outputs: ['main', 'main'],
  outputNames: ['true', 'false'],
  propertiesJson: jsonEncode([
    {
      'displayName': 'Mode',
      'name': 'mode',
      'type': 'options',
      'default': 'a',
      'options': [
        {'name': 'A', 'value': 'a'},
      ],
    },
    {
      'displayName': 'Extra',
      'name': 'extra',
      'type': 'string',
      'default': '',
      'displayOptions': {
        'show': {
          'mode': ['b'],
        },
      },
    },
  ]),
);

void main() {
  group('round trip', () {
    test('lossless incl. key order, unknown fields, floats, nulls', () {
      final wf = FlowWorkflow.parse(sample);
      expect(canon(wf.toJson()), canon(jsonDecode(sample)));
      expect(wf.nodes, hasLength(4));
      expect(wf.nodes[2].position, (x: 260.5, y: 40.0));
      expect(wf.nodes[0].raw['futureNodeField'], {
        'keep': ['me', 1, null],
      });
      // pretty export parses back to the same document
      expect(
        canon(FlowWorkflow.parse(wf.toJsonString()).toJson()),
        canon(jsonDecode(sample)),
      );
    });

    test('edits leave untouched nodes byte-identical', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      ed.moveNodes({'Done': (x: 300, y: 260)});
      final j = ed.workflow.toJson();
      final orig = jsonDecode(sample) as Map<String, dynamic>;
      expect(canon((j['nodes'] as List)[0]), canon((orig['nodes'] as List)[0]));
      expect(((j['nodes'] as List)[3] as Map)['position'], [300, 260]);
      expect(j['meta'], orig['meta']);
      expect(j.keys.toList(), orig.keys.toList());
    });

    test('rejects non-workflow json', () {
      expect(
        () => FlowWorkflow.parse('[1]'),
        throwsA(isA<FlowFormatException>()),
      );
      expect(
        () => FlowWorkflow.parse('{"a":1}'),
        throwsA(isA<FlowFormatException>()),
      );
      expect(
        () => FlowWorkflow.parse('nope'),
        throwsA(isA<FlowFormatException>()),
      );
    });

    test('validate flags dangling refs', () {
      final bad = jsonDecode(sample) as Map<String, dynamic>;
      (bad['connections'] as Map)['Ghost'] = {
        'main': [
          [
            {'node': 'If', 'type': 'main', 'index': 0},
          ],
        ],
      };
      expect(FlowWorkflow.fromJson(bad).validate(), isNotEmpty);
      expect(FlowWorkflow.parse(sample).validate(), isEmpty);
    });
  });

  group('connect / disconnect', () {
    test('connect adds edge and pads slots, disconnect prunes', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      const e = FlowEdge(src: 'Done', srcIndex: 1, dst: 'Notify', dstIndex: 0);
      expect(ed.connect(e), isTrue);
      expect(ed.workflow.connections.contains(e), isTrue);
      final slots = (ed.workflow.toJson()['connections'] as Map)['Done'] as Map;
      expect((slots['main'] as List).length, 2);
      expect((slots['main'] as List)[0], isNull);
      expect(ed.connect(e), isFalse, reason: 'duplicate');
      expect(ed.disconnect(e), isTrue);
      expect(
        (ed.workflow.toJson()['connections'] as Map).containsKey('Done'),
        isFalse,
      );
    });

    test('rejects self loop and unknown node', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      expect(
        ed.connect(
          const FlowEdge(src: 'If', srcIndex: 0, dst: 'If', dstIndex: 0),
        ),
        isFalse,
      );
      expect(
        ed.connect(
          const FlowEdge(src: 'If', srcIndex: 0, dst: 'Nope', dstIndex: 0),
        ),
        isFalse,
      );
      expect(ed.canUndo, isFalse);
    });

    test('delete node drops edges + pinData; rename rewrites refs', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      expect(ed.renameNode('If', 'Check'), isTrue);
      expect(ed.renameNode('Check', 'Done'), isFalse, reason: 'name taken');
      final conns = ed.workflow.connections;
      expect(conns.targets('Webhook', 'main', 0).single.node, 'Check');
      expect(conns.targets('Check', 'main', 1).single.node, 'Done');
      ed.renameNode('Webhook', 'Hook');
      expect(ed.workflow.pinData.keys, ['Hook']);
      ed.deleteNodes(['Check']);
      expect(ed.workflow.hasNode('Check'), isFalse);
      expect(ed.workflow.connections.edges, isEmpty);
      expect(ed.workflow.validate(), isEmpty);
    });
  });

  group('undo / redo', () {
    test('add, move, connect, delete round trip through history', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      final start = canon(ed.workflow.toJson());
      final name = ed.addNode(ifInfo(), (x: 10, y: 20), connectFrom: 'Done');
      expect(name, 'If1');
      expect(ed.workflow.node('If1')!.typeVersion, 2.2);
      expect(
        ed.workflow.connections.targets('Done', 'main', 0).single.node,
        'If1',
      );
      final added = canon(ed.workflow.toJson());

      ed.beginDrag(['If1']);
      ed.dragBy(5, 5);
      ed.dragBy(30, 40);
      ed.endDrag();
      expect(ed.workflow.node('If1')!.position, (x: 40.0, y: 60.0));

      ed.deleteNodes(['If1']);
      expect(ed.workflow.hasNode('If1'), isFalse);

      expect(ed.undo(), isTrue); // delete
      expect(ed.workflow.hasNode('If1'), isTrue);
      expect(ed.undo(), isTrue); // drag (one step, not three)
      expect(ed.workflow.node('If1')!.position, (x: 10.0, y: 20.0));
      expect(canon(ed.workflow.toJson()), added);
      expect(ed.undo(), isTrue); // add
      expect(canon(ed.workflow.toJson()), start);
      expect(ed.dirty, isFalse);
      expect(ed.undo(), isFalse);

      expect(ed.redo(), isTrue);
      expect(canon(ed.workflow.toJson()), added);
      expect(ed.redo(), isTrue);
      expect(ed.redo(), isTrue);
      expect(ed.workflow.hasNode('If1'), isFalse);
      expect(ed.redo(), isFalse);
    });

    test('typing burst with one coalesce key is one undo step', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      for (final v in ['a', 'ab', 'abc']) {
        ed.setParameters('Done', {'text': v}, coalesce: '/text');
      }
      expect(ed.workflow.node('Done')!.parameters['text'], 'abc');
      ed.setParameters('Done', {'text': 'x', 'other': 1}, coalesce: '/other');
      expect(ed.undo(), isTrue);
      expect(ed.workflow.node('Done')!.parameters['text'], 'abc');
      expect(ed.undo(), isTrue);
      expect(ed.workflow.node('Done')!.parameters, isEmpty);
      expect(ed.undo(), isFalse);
    });

    test('new edit clears redo; no-op drag adds no history', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      ed.beginDrag(['Done']);
      ed.endDrag();
      expect(ed.canUndo, isFalse);
      ed.setDisabled('Done', true);
      ed.undo();
      expect(ed.canRedo, isTrue);
      ed.setNotes('Done', 'x');
      expect(ed.canRedo, isFalse);
    });
  });

  group('clipboard', () {
    test('copy selection keeps only inner edges, paste remaps', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      ed.select(['Webhook', 'If']);
      final text = ed.copySelection()!;
      final j = jsonDecode(text) as Map<String, dynamic>;
      expect((j['nodes'] as List).length, 2);
      expect((j['connections'] as Map).keys.toList(), [
        'Webhook',
      ], reason: 'If -> Notify/Done leave the selection');
      expect((j['pinData'] as Map).keys, ['Webhook']);

      final pasted = ed.paste(text);
      expect(pasted, ['Webhook1', 'If1']);
      expect(ed.selection, {'Webhook1', 'If1'});
      final wf = ed.workflow;
      expect(wf.connections.targets('Webhook1', 'main', 0).single.node, 'If1');
      expect(wf.node('If1')!.id, isNot('22222222-2222-4222-8222-222222222222'));
      expect(
        wf.node('Webhook1')!.raw['webhookId'],
        isNot('aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'),
      );
      expect(wf.node('If1')!.position, (x: 40.0, y: 160.0));
      expect(wf.node('If1')!.raw['credentials'], isNotNull);
      expect(wf.pinData.keys, containsAll(['Webhook', 'Webhook1']));
      expect(wf.validate(), isEmpty);
      expect(ed.undo(), isTrue);
      expect(ed.workflow.nodes, hasLength(4));
    });

    test('paste at position moves block top-left there', () {
      final ed = FlowEditor(FlowWorkflow.empty());
      final names = ed.paste(sample, at: (x: 100, y: 100));
      expect(names, hasLength(4));
      expect(ed.workflow.node('Webhook')!.position, (x: 100.0, y: 180.0));
      expect(
        ed.workflow.node('Notify')!.position.x,
        601,
        reason: 'ints on export',
      );
      expect(ed.workflow.connections.edges, hasLength(3));
    });

    test('bare node array, single node and junk', () {
      final ed = FlowEditor(FlowWorkflow.empty());
      expect(
        ed.paste(
          '[{"name":"A","type":"n8n-nodes-base.noOp","typeVersion":1,"position":[0,0]}]',
        ),
        ['A'],
      );
      expect(
        ed.paste(
          '{"name":"A","type":"n8n-nodes-base.noOp","typeVersion":1,"position":[0,0]}',
        ),
        ['A1'],
      );
      expect(ed.paste('hello world'), isEmpty);
      expect(ed.paste('{"foo": 1}'), isEmpty);
      expect(
        () => ed.paste('{"nodes":[{"x":1}]}'),
        throwsA(isA<FlowFormatException>()),
      );
      expect(
        () => ed.paste('{"nodes":[]}'),
        throwsA(isA<FlowFormatException>()),
      );
    });

    test(
      'import workflow replaces doc as one undo step; export parses back',
      () {
        final ed = FlowEditor();
        ed.importWorkflow(sample);
        expect(ed.workflow.name, 'Triage');
        expect(ed.workflow.nodes, hasLength(4));
        expect(
          canon(FlowWorkflow.parse(ed.exportWorkflow()).toJson()),
          canon(jsonDecode(sample)),
        );
        ed.undo();
        expect(ed.workflow.nodes, isEmpty);
      },
    );

    test('cut removes selection and returns json', () {
      final ed = FlowEditor(FlowWorkflow.parse(sample));
      ed.select(['Done']);
      final cut = ed.cutSelection()!;
      expect(ed.workflow.hasNode('Done'), isFalse);
      expect(jsonDecode(cut)['nodes'], hasLength(1));
    });
  });

  group('node types', () {
    test('displayOptions visibility', () {
      final p = ifInfo().properties[1];
      expect(p.isVisible({'mode': 'a'}), isFalse);
      expect(p.isVisible({'mode': 'b'}), isTrue);
    });

    test('ports: static, expression and unsupported', () {
      final wf = FlowWorkflow.parse(sample);
      final info = ifInfo();
      expect(outputPorts(wf.node('If')!, info, 1).map((p) => p.label), [
        'true',
        'false',
      ]);
      expect(inputPorts(wf.node('Webhook')!, null), isEmpty, reason: 'trigger');
      expect(inputPorts(wf.node('Notify')!, null), hasLength(1));
      expect(outputPorts(wf.node('Notify')!, null, -1), hasLength(1));

      final merge = NodeTypeInfo.fromFields(
        typeName: 'n8n-nodes-base.merge',
        versions: [3],
        defaultVersion: 3,
        displayName: 'Merge',
        description: '',
        group: ['transform'],
        inputs: [],
        outputs: ['main'],
        inputsExpr:
            r"={{ Array(($parameter.numberInputs || 2)).fill('main') }}",
        propertiesJson: '[]',
      );
      final node = FlowNode.create(
        id: 'x',
        name: 'M',
        type: merge.typeName,
        typeVersion: 3,
        position: (x: 0, y: 0),
        parameters: {'numberInputs': 4},
      );
      expect(inputPorts(node, merge), hasLength(4));
      expect(merge.supportsVersion(3), isTrue);
      expect(merge.supportsVersion(2), isFalse);
    });

    test('categories', () {
      expect(categoryOf('n8n-nodes-base.webhook'), NodeCategory.trigger);
      expect(categoryOf('n8n-nodes-base.if'), NodeCategory.logic);
      expect(categoryOf('@n8n/n8n-nodes-langchain.agent'), NodeCategory.ai);
      expect(
        categoryOf('n8n-nodes-base.httpRequest'),
        NodeCategory.integration,
      );
    });
  });

  test('flow store: create, list, rename, delete', () async {
    final dir = await Directory.systemTemp.createTemp('hk_flows_test');
    addTearDown(() => dir.delete(recursive: true));
    final store = FlowStore(dir);
    final a = await store.create('Alpha');
    await store.save(a.id, FlowWorkflow.parse(sample));
    expect((await store.list()).map((f) => f.name), ['Triage']);
    await store.rename(a.id, 'Renamed');
    final loaded = await store.load(a.id);
    expect(loaded.name, 'Renamed');
    expect(loaded.nodes, hasLength(4));
    await store.delete(a.id);
    expect(await store.list(), isEmpty);
  });
}
