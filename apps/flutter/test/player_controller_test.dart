import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/studio/player_controller.dart';
import 'package:hokkabaz/screens/studio/studio_models.dart';

class FakeBackend implements PlayerBackend {
  double pos = 0;
  bool isPlaying = false;
  final seeks = <double>[];
  bool disposed = false;

  @override
  Future<void> open(String path) async {}
  @override
  Future<void> play() async => isPlaying = true;
  @override
  Future<void> pause() async => isPlaying = false;
  @override
  Future<void> setRate(double rate) async {}
  @override
  Future<void> seek(double seconds) async {
    pos = seconds;
    seeks.add(seconds);
  }

  @override
  double get position => pos;
  @override
  bool get playing => isPlaying;
  @override
  Future<void> dispose() async => disposed = true;
}

PreviewPlan plan(String clip, List<(double, double)> ranges) {
  final d = ranges.fold<double>(0, (a, r) => a + r.$2 - r.$1);
  return PreviewPlan(
    clipId: clip,
    srcStart: ranges.first.$1,
    srcEnd: ranges.last.$2,
    duration: d,
    ranges: ranges,
    words: const [],
    captions: const [],
    captionStyle: null,
    crop: null,
    outW: 1920,
    outH: 1080,
  );
}

void main() {
  test('loop restarts at the first range instead of stopping', () async {
    final b = FakeBackend();
    final c = EdlPlayerController(b, tickEvery: const Duration(hours: 1));
    c.setPlan(plan('a', [(1.0, 3.0)]));
    c.toggleLoop();
    await c.play();
    b.pos = 3.0;
    c.tick();
    await Future<void>.delayed(Duration.zero);
    expect(c.playing, isTrue);
    expect(b.seeks.last, 1.0);
    expect(c.outTime.value, 0);
  });

  test('plays across a cut and stops at the end', () async {
    final b = FakeBackend();
    final c = EdlPlayerController(b, tickEvery: const Duration(hours: 1));
    c.setPlan(plan('a', [(1.0, 3.0), (5.0, 6.0)]));
    expect(b.seeks.last, 1.0, reason: 'new clip starts at its first range');
    await c.play();
    expect(c.playing, isTrue);

    b.pos = 2.0;
    c.tick();
    expect(c.outTime.value, closeTo(1.0, 1e-9));

    b.pos = 3.0; // end of range 0: jump over the cut
    c.tick();
    expect(b.seeks.last, 5.0);
    expect(c.outTime.value, closeTo(2.0, 1e-9));

    b.pos = 5.5;
    c.tick();
    expect(c.outTime.value, closeTo(2.5, 1e-9));

    b.pos = 6.0; // end of last range
    c.tick();
    await Future<void>.delayed(Duration.zero);
    expect(c.playing, isFalse);
    expect(c.outTime.value, closeTo(3.0, 1e-9));

    // Pressing play at the end restarts.
    await c.play();
    expect(c.outTime.value, closeTo(0.0, 1e-9));
    expect(b.seeks.last, 1.0);
    c.dispose();
    expect(b.disposed, isTrue);
  });

  test('seek and edits keep the playhead where it is', () async {
    final b = FakeBackend();
    final c = EdlPlayerController(b, tickEvery: const Duration(hours: 1));
    c.setPlan(plan('a', [(0.0, 10.0)]));
    await c.seekOut(4.0);
    expect(b.seeks.last, 4.0);
    expect(c.outTime.value, 4.0);

    // The same clip gets a cut before the playhead: output 4.0 now maps to source 6.0.
    c.setPlan(plan('a', [(0.0, 2.0), (4.0, 10.0)]));
    expect(c.outTime.value, 4.0);
    await Future<void>.delayed(Duration.zero);
    expect(b.seeks.last, 6.0);

    // A different clip resets to its start and pauses.
    await c.play();
    c.setPlan(plan('b', [(20.0, 30.0)]));
    expect(c.playing, isFalse);
    expect(c.outTime.value, 0.0);
    expect(b.seeks.last, 20.0);

    await c.seekSource(25.0);
    expect(c.outTime.value, 5.0);
    c.setPlan(null);
    expect(c.duration, 0);
    c.dispose();
  });
}
