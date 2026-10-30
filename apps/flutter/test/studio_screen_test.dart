import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/studio/player_controller.dart';
import 'package:hokkabaz/screens/studio/agent_panel.dart';
import 'package:hokkabaz/screens/studio/studio_models.dart';
import 'package:hokkabaz/screens/studio/studio_providers.dart';
import 'package:hokkabaz/screens/studio/studio_screen.dart';
import 'package:hokkabaz/theme/theme.dart';

class FakeBackend implements PlayerBackend {
  @override
  Future<void> open(String path) async {}
  @override
  Future<void> play() async {}
  @override
  Future<void> pause() async {}
  @override
  Future<void> setRate(double rate) async {}
  @override
  Future<void> seek(double seconds) async {}
  @override
  double get position => 0;
  @override
  bool get playing => false;
  @override
  Future<void> dispose() async {}
}

final recorded = <List<Map<String, dynamic>>>[];

class FakeController extends StudioController {
  FakeController(super.projectId, this.initial);

  final StudioState initial;

  @override
  StudioState build() => initial;

  @override
  Future<bool> edit(List<Map<String, dynamic>> ops) async {
    recorded.add(ops);
    return true;
  }

  @override
  Future<void> selectClip(String? id) async {}
}

StudioState fakeState() {
  final project = StudioProjectData.parse(jsonEncode({
    'id': 'p1',
    'name': 'Podcast ep. 12',
    'has_transcript': true,
    'source': {'path': '/v.mp4', 'duration': 600.0, 'width': 1920, 'height': 1080, 'fps': 30.0, 'has_audio': true},
    'clips': [
      {
        'id': 'c1',
        'title': 'The big reveal',
        'start': 30.0,
        'end': 62.0,
        'deleted_words': [3],
        'remove_fillers': false,
        'captions': {'font': 'Arial', 'size': 72, 'primary': '#FFFFFF', 'highlight': '#FFE600', 'mode': 'karaoke', 'pos': 'center', 'uppercase': true, 'max_words': 4, 'max_chars': 22, 'max_gap': 0.8},
        'reframe': {'aspect': [9, 16], 'size': [1080, 1920], 'mode': 'track', 'smoothing': 0.5, 'zoom': 1.0, 'focus': [{'t': 30, 'x': 0.4, 'y': 0.5}]},
        'broll': [],
        'score': {'total': 82, 'hook': 90, 'emotion': 70, 'novelty': 60, 'clarity': 85, 'pacing': 80, 'shareability': 88, 'platform_fit': 90, 'summary': 'Strong cold open.', 'strengths': ['hook'], 'improvements': ['trim the intro']},
        'social': {'posts': {'tiktok': {'title': 'You won\'t believe this', 'description': 'A reveal.', 'hashtags': ['#podcast']}}},
        'template': 'shorts-karaoke',
      },
      {'id': 'c2', 'title': 'Second', 'start': 100.0, 'end': 130.0},
    ],
  }));
  final words = [
    for (var i = 0; i < 40; i++)
      PreviewWord(
        idx: i,
        text: i == 5 ? 'um' : 'word$i${i % 9 == 8 ? '.' : ''}',
        start: 30 + i * 0.5,
        end: 30 + i * 0.5 + 0.4,
        deleted: i == 3,
        filler: i == 5,
        outStart: i == 3 ? null : i * 0.5,
        outEnd: i == 3 ? null : i * 0.5 + 0.4,
      ),
  ];
  final plan = PreviewPlan(
    clipId: 'c1',
    srcStart: 30,
    srcEnd: 62,
    duration: 19.5,
    ranges: const [(30.0, 31.5), (32.0, 50.0)],
    words: words,
    captions: [for (final w in words.where((w) => !w.deleted)) CaptionWord(w.text, w.outStart!, w.outEnd!)],
    captionStyle: project.clip('c1')!.captions,
    crop: const CropPath([CropKey(0, 600, 0, 608, 1080)], smooth: false),
    outW: 1080,
    outH: 1920,
  );
  return StudioState(project: project, plan: plan, selectedClip: 'c1', canUndo: true, loading: false);
}

Widget app(Size size) {
  final state = fakeState();
  return ProviderScope(
    overrides: [
      studioControllerProvider.overrideWith2((id) => FakeController(id, state)),
      studioCatalogProvider.overrideWith((ref) async => {
        'templates': [
          {'name': 'shorts-karaoke', 'description': 'd'},
          {'name': 'clean-vertical', 'description': 'd'},
        ],
        'caption_presets': ['classic', 'karaoke', 'pop', 'minimal'],
      }),
      studioToolsProvider.overrideWith((ref) async => {'ffmpeg': true}),
      studioAgentSessionProvider.overrideWith((ref, id) async => throw StateError('offline in test')),
    ],
    child: MaterialApp(
      theme: buildTheme(Brightness.light),
      home: Scaffold(body: ProjectView(projectId: 'p1', backendFactory: FakeBackend.new)),
    ),
  );
}

void main() {
  setUp(recorded.clear);

  for (final (name, size) in [('desktop', const Size(1500, 900)), ('phone', const Size(390, 844))]) {
    testWidgets('project view lays out without overflow on $name', (tester) async {
      tester.view.physicalSize = size;
      tester.view.devicePixelRatio = 1;
      addTearDown(tester.view.reset);
      await tester.pumpWidget(app(size));
      await tester.pump(const Duration(milliseconds: 300));
      expect(tester.takeException(), isNull);
      expect(find.text('Podcast ep. 12'), findsOneWidget);
      if (name == 'phone') {
        // Walk every tab; each must lay out.
        for (final tab in ['Clips', 'Edit', 'Style', 'Publish', 'Agent']) {
          await tester.tap(find.widgetWithText(Tab, tab));
          await tester.pump(const Duration(milliseconds: 500));
          await tester.pump(const Duration(milliseconds: 500));
          expect(tester.takeException(), isNull, reason: tab);
        }
      }
    });
  }

  testWidgets('selecting words and pressing Delete sends a delete_words edit', (tester) async {
    tester.view.physicalSize = const Size(1500, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(app(const Size(1500, 900)));
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.text('word6'));
    await tester.pump();
    await tester.tap(find.widgetWithText(FilledButton, 'Delete'));
    await tester.pump();
    expect(recorded.single.single['op'], 'delete_words');
    expect(recorded.single.single['words'], [6]);
  });

  testWidgets('style panel edits send ops', (tester) async {
    tester.view.physicalSize = const Size(1500, 900);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(app(const Size(1500, 900)));
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.text('Captions').first);
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pump(const Duration(milliseconds: 500));
    await tester.tap(find.text('pop'));
    await tester.pump();
    expect(recorded.last.single['op'], 'set_caption_preset');
    expect(recorded.last.single['preset'], 'pop');
    await tester.tap(find.text('Word'));
    await tester.pump();
    expect(recorded.last.single['op'], 'set_captions');
    expect((recorded.last.single['style'] as Map)['mode'], 'word_by_word');
  });

  testWidgets('publish panel shows the score and post copy', (tester) async {
    tester.view.physicalSize = const Size(1500, 1400);
    tester.view.devicePixelRatio = 1;
    addTearDown(tester.view.reset);
    await tester.pumpWidget(app(const Size(1500, 1400)));
    await tester.pump(const Duration(milliseconds: 300));
    await tester.tap(find.text('Publish').last);
    await tester.pump(const Duration(milliseconds: 500));
    await tester.pump(const Duration(milliseconds: 500));
    expect(find.text('Strong cold open.'), findsOneWidget);
    expect(find.textContaining("won't believe", findRichText: true), findsOneWidget);
    expect(find.text('Render clip'), findsOneWidget);
  });
}
