import 'dart:io';

import 'package:media_kit/media_kit.dart' as mk;
import 'package:path_provider/path_provider.dart';
import 'package:record/record.dart';
import 'package:video_player/video_player.dart';

import 'hk_gate.dart';

/// Push-to-talk recording → speech-to-text (Studio STT engine), and read-aloud via TTS.
class Voice {
  Voice._();
  static final Voice instance = Voice._();

  final AudioRecorder _rec = AudioRecorder();
  bool _mkInit = false;
  mk.Player? _player;
  VideoPlayerController? _android;

  /// Start recording to a temp WAV (16 kHz mono). Throws when the mic is not allowed.
  Future<void> start() async {
    if (!await _rec.hasPermission()) {
      throw Exception('Microphone permission denied');
    }
    final dir = await getTemporaryDirectory();
    final path =
        '${dir.path}${Platform.pathSeparator}hk_voice_${DateTime.now().millisecondsSinceEpoch}.wav';
    await _rec.start(
      const RecordConfig(
        encoder: AudioEncoder.wav,
        sampleRate: 16000,
        numChannels: 1,
      ),
      path: path,
    );
  }

  Future<bool> get recording => _rec.isRecording();

  /// Stop and transcribe; returns the text (empty when nothing was said).
  Future<String> stopAndTranscribe() async {
    final path = await _rec.stop();
    if (path == null) return '';
    try {
      final t = await hk('studio.transcribe_file', {'path': path});
      return (t as String?)?.trim() ?? '';
    } finally {
      try {
        await File(path).delete();
      } catch (_) {}
    }
  }

  /// Speak `text` with the configured TTS engine (AI hub → AI settings → Generation).
  Future<void> speak(String text) async {
    await stopSpeaking();
    final path = await hk('tts.speak', {'text': text}) as String?;
    if (path == null) return;
    if (Platform.isAndroid) {
      final c = VideoPlayerController.file(File(path));
      _android = c;
      await c.initialize();
      await c.play();
    } else {
      if (!_mkInit) {
        mk.MediaKit.ensureInitialized();
        _mkInit = true;
      }
      final p = _player ??= mk.Player();
      await p.open(mk.Media(path));
    }
  }

  Future<void> stopSpeaking() async {
    await _player?.stop();
    final a = _android;
    _android = null;
    await a?.dispose();
  }
}
