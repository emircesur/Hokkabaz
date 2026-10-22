import 'dart:convert';

import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/studio/studio_models.dart';

void main() {
  group('EdlClock', () {
    final c = EdlClock([(1.0, 3.0), (5.0, 6.0)]);

    test('duration and mapping', () {
      expect(c.duration, 3.0);
      expect(c.toSource(0.5), 1.5);
      expect(c.toSource(2.5), 5.5);
      expect(c.toSource(99), 6.0);
      expect(c.toOutput(1.5), 0.5);
      expect(c.toOutput(5.5), 2.5);
      expect(c.toOutput(4.0), 2.0); // cut-out time snaps forward
      expect(c.toOutput(0.0), 0.0);
      expect(c.toOutput(10.0), 3.0);
    });

    test('range lookup', () {
      expect(c.rangeAt(0.0), 0);
      expect(c.rangeAt(1.99), 0);
      expect(c.rangeAt(2.0), 1);
      expect(c.rangeStartOut(1), 2.0);
      expect(c.rangeEndOut(1), 3.0);
    });

    test('empty clock is safe', () {
      final e = EdlClock(const []);
      expect(e.duration, 0);
      expect(e.toSource(1), 0);
      expect(e.rangeAt(1), 0);
    });
  });

  group('CropPath', () {
    test('linear and smooth interpolation', () {
      final lin = CropPath.fromJson({
        'interp': 'linear',
        'keys': [
          {'t': 0, 'x': 0, 'y': 0, 'w': 100, 'h': 200},
          {'t': 2, 'x': 100, 'y': 0, 'w': 100, 'h': 200},
        ],
      });
      expect(lin.at(1).x, closeTo(50, 1e-9));
      expect(lin.at(-1).x, 0);
      expect(lin.at(5).x, 100);
      final sm = CropPath(lin.keys, smooth: true);
      expect(sm.at(0.5).x, lessThan(25)); // eased in
      expect(sm.at(1).x, closeTo(50, 1e-9));
    });
  });

  test('parses a project and a preview plan', () {
    final project = StudioProjectData.parse(jsonEncode({
      'id': 'p1',
      'name': 'Demo',
      'has_transcript': true,
      'source': {'path': '/v.mp4', 'duration': 60.0, 'width': 1920, 'height': 1080, 'fps': 30.0, 'has_audio': true},
      'clips': [
        {
          'id': 'c1',
          'title': 'Opening',
          'start': 1.0,
          'end': 20.0,
          'deleted_words': [3, 4],
          'remove_fillers': true,
          'reframe': {'aspect': [9, 16], 'focus': [{'t': 0, 'x': .5, 'y': .5}]},
          'broll': [{}, {}],
          'score': {'total': 71, 'hook': 80, 'emotion': 60, 'novelty': 40, 'clarity': 100, 'pacing': 50, 'shareability': 70, 'platform_fit': 90, 'summary': 'ok', 'strengths': ['a'], 'improvements': ['b']},
          'social': {'posts': {'tiktok': {'title': 'T'}}},
        },
      ],
    }));
    final c = project.clip('c1')!;
    expect(project.source.width, 1920);
    expect(c.deletedWords, {3, 4});
    expect(c.aspectName, 'vertical');
    expect(c.tracked, isTrue);
    expect(c.brollCount, 2);
    expect(c.score!.total, 71);
    expect(c.score!.criteria['platform_fit'], 90);
    expect(c.social!.keys, ['tiktok']);
    expect(project.clip('nope'), isNull);

    final plan = PreviewPlan.parse(jsonEncode({
      'clip_id': 'c1',
      'src_start': 1.0,
      'src_end': 20.0,
      'duration': 10.0,
      'ranges': [[1.0, 6.0], [8.0, 13.0]],
      'words': [
        {'idx': 0, 'text': 'hi', 'start': 1.0, 'end': 1.4, 'deleted': false, 'filler': false, 'out_start': 0.0, 'out_end': 0.4},
        {'idx': 1, 'text': 'um', 'start': 6.0, 'end': 6.4, 'deleted': true, 'filler': true, 'out_start': null, 'out_end': null},
      ],
      'captions': [{'text': 'hi', 'start': 0.0, 'end': 0.4}],
      'caption_style': null,
      'crop': null,
      'out_size': [1080, 1920],
    }));
    expect(plan.ranges.length, 2);
    expect(plan.clock.duration, 10.0);
    expect(plan.words[1].deleted, isTrue);
    expect(plan.words[1].outStart, isNull);
    expect(plan.aspect, closeTo(9 / 16, 1e-9));
  });

  test('parses events', () {
    final e = StudioEvent.parse('{"kind":"changed","rev":4,"clips":["c1"],"by":"agent"}');
    expect(e.kind, 'changed');
    expect(e.by, 'agent');
    expect(e.clips, ['c1']);
    final p = StudioEvent.parse('{"kind":"render_progress","clip":"c1","fraction":0.5}');
    expect(p.fraction, 0.5);
    expect(formatTime(75.34), '1:15.3');
  });
}
