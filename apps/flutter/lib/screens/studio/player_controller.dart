import 'dart:async';

import 'package:flutter/foundation.dart';

import 'studio_models.dart';

/// What the editor needs from a video engine. Positions are SOURCE seconds.
abstract class PlayerBackend {
  Future<void> open(String path);
  Future<void> play();
  Future<void> pause();
  Future<void> seek(double seconds);

  /// Best-effort current position (extrapolated between engine updates).
  double get position;
  bool get playing;
  Future<void> dispose();
}

/// Plays the kept ranges of a [PreviewPlan] back to back on top of a plain
/// source-video player: the edit is visible and audible instantly, no render.
class EdlPlayerController extends ChangeNotifier {
  EdlPlayerController(this.backend, {this.tickEvery = const Duration(milliseconds: 33)});

  final PlayerBackend backend;
  final Duration tickEvery;
  Timer? _timer;
  PreviewPlan? _plan;
  EdlClock _clock = EdlClock(const []);
  int _range = 0;
  bool _playing = false;
  bool _disposed = false;

  /// Output-timeline playhead.
  final ValueNotifier<double> outTime = ValueNotifier(0);

  PreviewPlan? get plan => _plan;
  bool get playing => _playing;
  double get duration => _clock.duration;

  void setPlan(PreviewPlan? plan) {
    final sameClip = plan != null && _plan?.clipId == plan.clipId;
    final keep = outTime.value;
    _plan = plan;
    _clock = plan?.clock ?? EdlClock(const []);
    if (plan == null || _clock.duration == 0) {
      pause();
      outTime.value = 0;
    } else if (!sameClip) {
      pause();
      seekOut(0);
    } else {
      final t = keep.clamp(0.0, _clock.duration);
      outTime.value = t;
      _range = _clock.rangeAt(t);
      // An edit before the playhead moves what is under it: re-sync the engine.
      final want = _clock.toSource(t);
      if ((backend.position - want).abs() > 0.2) unawaited(backend.seek(want));
    }
    notifyListeners();
  }

  Future<void> seekOut(double t) async {
    if (_clock.duration == 0) return;
    final c = t.clamp(0.0, _clock.duration);
    outTime.value = c;
    _range = _clock.rangeAt(c);
    await backend.seek(_clock.toSource(c));
  }

  /// Seek to the playhead position of source time [s] (cut-out moments snap forward).
  Future<void> seekSource(double s) => seekOut(_clock.toOutput(s));

  Future<void> play() async {
    if (_playing || _clock.duration == 0) return;
    if (outTime.value >= _clock.duration - 0.05) await seekOut(0);
    _playing = true;
    await backend.play();
    _timer?.cancel();
    _timer = Timer.periodic(tickEvery, (_) => tick());
    notifyListeners();
  }

  Future<void> pause() async {
    _timer?.cancel();
    _timer = null;
    if (_playing) {
      _playing = false;
      await backend.pause();
      notifyListeners();
    }
  }

  Future<void> toggle() => _playing ? pause() : play();

  /// One clock step; public so tests can drive it without a timer.
  void tick() {
    if (!_playing || _disposed || _plan == null || _clock.ranges.isEmpty) return;
    final pos = backend.position;
    final r = _clock.ranges[_range];
    if (pos >= r.$2 - 0.02) {
      if (_range + 1 < _clock.ranges.length) {
        _range++;
        final next = _clock.ranges[_range];
        outTime.value = _clock.rangeStartOut(_range);
        unawaited(backend.seek(next.$1));
      } else {
        outTime.value = _clock.duration;
        unawaited(pause());
      }
      return;
    }
    outTime.value = (_clock.rangeStartOut(_range) + (pos - r.$1)).clamp(0.0, _clock.duration);
  }

  @override
  void dispose() {
    _disposed = true;
    _timer?.cancel();
    outTime.dispose();
    unawaited(backend.dispose());
    super.dispose();
  }
}
