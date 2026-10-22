import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/flows/model/node_types.dart';
import 'package:hokkabaz/screens/flows/model/workflow_model.dart';
import 'package:hokkabaz/screens/plugins/plugins_models.dart';

void main() {
  test('plugin list json', () {
    final raw = jsonEncode([
      {
        'id': 'dev.x.y',
        'name': 'Y',
        'version': '1.0.0',
        'enabled': true,
        'loaded': true,
        'capabilities': ['HTTP: api.example.com'],
        'tools': ['t1'],
        'nodes': ['N'],
        'triggers': [],
        'settings_schema': {'description': 'hi'},
        'settings': {'a': 1},
      },
    ]);
    final p = PluginInfo.parseList(raw).single;
    expect(p.capabilities, ['HTTP: api.example.com']);
    expect(p.settingsSchema!['description'], 'hi');
    expect(p.settings['a'], 1);
    expect(p.error, isNull);
  });

  test('credential types keep only editable fields and flag secrets', () {
    final raw = jsonEncode([
      {
        'name': 'slackApi',
        'display_name': 'Slack API',
        'fields': [
          {'name': 'accessToken', 'display_name': 'Access Token', 'type': 'string', 'required': true, 'secret': true},
          {'name': 'notice', 'display_name': 'x', 'type': 'notice'},
          {'name': 'mode', 'display_name': 'Mode', 'type': 'options', 'options': [{'name': 'A', 'value': 'a'}]},
        ],
      },
    ]);
    final t = CredType.parseList(raw).single;
    expect(t.fields.map((f) => f.name), ['accessToken', 'mode']);
    expect(t.fields.first.secret, isTrue);
    expect(t.fields.last.options.single.$1, 'a');
  });

  test('node credential requirements follow displayOptions', () {
    final info = NodeTypeInfo.fromFields(
      typeName: 'n8n-nodes-base.x',
      versions: const [1],
      defaultVersion: 1,
      displayName: 'X',
      description: '',
      group: const [],
      inputs: const ['main'],
      outputs: const ['main'],
      propertiesJson: '[]',
      credentialsJson: jsonEncode([
        {'name': 'a', 'required': true, 'displayOptions': {'show': {'auth': ['basic']}}},
        {'name': 'b'},
      ]),
    );
    expect(info.credentials.length, 2);
    expect(info.credentials[0].isVisible({'auth': 'basic'}), isTrue);
    expect(info.credentials[0].isVisible({'auth': 'none'}), isFalse);
    expect(info.credentials[1].isVisible({}), isTrue);
  });

  test('FlowNode credential references round-trip', () {
    var n = FlowNode.create(id: '1', name: 'N', type: 't', typeVersion: 1, position: (x: 0, y: 0));
    n = n.withCredential('slackApi', id: 'c1', name: 'Team');
    expect(n.credentials['slackApi'], {'id': 'c1', 'name': 'Team'});
    n = n.withCredential('slackApi');
    expect(n.raw.containsKey('credentials'), isFalse);
  });
}
