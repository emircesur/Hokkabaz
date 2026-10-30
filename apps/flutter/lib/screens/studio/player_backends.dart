import 'dart:async';
import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:media_kit/media_kit.dart' as mk;
import 'package:media_kit_video/media_kit_video.dart' as mkv;
import 'package:video_player/video_player.dart';

import 'player_controller.dart';

/// Wraps an engine's coarse position updates (they arrive a few times a second)
/// with a stopwatch so cuts land on time.
mixin _Extrapolate {
  double _base = 0;
  final Stopwatch _sw = Stopwatch();
  double _limit = double.infinity;
  bool _isPlaying = false;
  double _rate = 1;

  void _update(double pos, {required bool playing, double? limit}) {
    _base = pos;
    _isPlaying = playing;
    if (limit != null) _limit = limit;
    if (playing) {
      _sw.reset();
      _sw.start();
    } else {
      _sw.stop();
    }
  }

  double get _now => _isPlaying
      ? (_base + _sw.elapsedMicroseconds / 1e6 * _rate).clamp(0.0, _limit)
      : _base;
}

/// Engine for Android / iOS / macOS (official `video_player`).
class VideoPlayerBackend with _Extrapolate implements PlayerBackend {
  VideoPlayerController? _c;

  VideoPlayerController? get controller => _c;

  @override
  Future<void> open(String path) async {
    await _c?.dispose();
    _c = null;
    final c = path.startsWith('http')
        ? VideoPlayerController.networkUrl(Uri.parse(path))
        : VideoPlayerController.file(File(path));
    await c.initialize();
    await c.setLooping(false);
    c.addListener(() {
      final v = c.value;
      // Re-anchor only when the engine reports something the extrapolation did not predict.
      final p = v.position.inMicroseconds / 1e6;
      if ((p - _now).abs() > 0.08 || v.isPlaying != _isPlaying) {
        _update(
          p,
          playing: v.isPlaying,
          limit: v.duration.inMicroseconds / 1e6,
        );
      }
    });
    _c = c;
    _update(0, playing: false, limit: c.value.duration.inMicroseconds / 1e6);
  }

  @override
  Future<void> play() async {
    _update(_now, playing: true);
    await _c?.play();
  }

  @override
  Future<void> pause() async {
    _update(_now, playing: false);
    await _c?.pause();
  }

  @override
  Future<void> seek(double seconds) async {
    _update(seconds, playing: _isPlaying);
    await _c?.seekTo(Duration(microseconds: (seconds * 1e6).round()));
  }

  @override
  Future<void> setRate(double rate) async {
    _update(_now, playing: _isPlaying);
    _rate = rate;
    await _c?.setPlaybackSpeed(rate);
  }

  @override
  double get position => _now;

  @override
  bool get playing => _isPlaying;

  Widget view() => _c == null ? const SizedBox.shrink() : VideoPlayer(_c!);

  @override
  Future<void> dispose() async => _c?.dispose();
}

/// Engine for Linux / Windows (libmpv through `media_kit`).
class MediaKitBackend with _Extrapolate implements PlayerBackend {
  mk.Player? _p;
  mkv.VideoController? _vc;
  StreamSubscription<Duration>? _posSub;
  static bool _inited = false;

  @override
  Future<void> open(String path) async {
    if (!_inited) {
      mk.MediaKit.ensureInitialized();
      _inited = true;
    }
    // Re-open (new source / proxy): release the previous player.
    await _posSub?.cancel();
    await _p?.dispose();
    final p = mk.Player();
    _p = p;
    _vc = mkv.VideoController(p);
    await p.open(mk.Media(path), play: false);
    _posSub = p.stream.position.listen((d) {
      final pos = d.inMicroseconds / 1e6;
      if ((pos - _now).abs() > 0.08) _update(pos, playing: _isPlaying);
    });
    _update(0, playing: false);
  }

  @override
  Future<void> play() async {
    _update(_now, playing: true);
    await _p?.play();
  }

  @override
  Future<void> pause() async {
    _update(_now, playing: false);
    await _p?.pause();
  }

  @override
  Future<void> seek(double seconds) async {
    _update(seconds, playing: _isPlaying);
    await _p?.seek(Duration(microseconds: (seconds * 1e6).round()));
  }

  @override
  Future<void> setRate(double rate) async {
    _update(_now, playing: _isPlaying);
    _rate = rate;
    await _p?.setRate(rate);
  }

  @override
  double get position => _now;

  @override
  bool get playing => _isPlaying;

  Widget view() => _vc == null
      ? const SizedBox.shrink()
      : mkv.Video(
          controller: _vc!,
          controls: mkv.NoVideoControls,
          fill: const Color(0x00000000),
        );

  @override
  Future<void> dispose() async {
    await _posSub?.cancel();
    await _p?.dispose();
  }
}

/// Best engine for this platform.
PlayerBackend createBackend() => (Platform.isLinux || Platform.isWindows)
    ? MediaKitBackend()
    : VideoPlayerBackend();

/// Video surface of any backend created by [createBackend].
Widget backendView(PlayerBackend b) => switch (b) {
  VideoPlayerBackend() => b.view(),
  MediaKitBackend() => b.view(),
  _ => const SizedBox.shrink(),
};
