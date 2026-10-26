import 'dart:convert';

/// Dart view of the Studio JSON the Rust core returns. Objects the UI edits
/// whole (caption style, reframe spec) stay as raw maps so nothing is lost on
/// the way back.
typedef Json = Map<String, dynamic>;

double _d(Object? v, [double or = 0]) => v is num ? v.toDouble() : or;
int _i(Object? v, [int or = 0]) => v is num ? v.toInt() : or;
List<T> _list<T>(Object? v, T Function(Json) f) => (v as List? ?? const [])
    .map((e) => f((e as Map).cast<String, dynamic>()))
    .toList();

class ProjectSummary {
  const ProjectSummary({
    required this.id,
    required this.name,
    required this.duration,
    required this.width,
    required this.height,
    required this.clips,
    required this.hasTranscript,
    required this.createdMs,
  });

  factory ProjectSummary.fromJson(Json j) => ProjectSummary(
    id: j['id'] as String,
    name: j['name'] as String? ?? '',
    duration: _d(j['duration']),
    width: _i(j['width']),
    height: _i(j['height']),
    clips: _i(j['clips']),
    hasTranscript: j['has_transcript'] == true,
    createdMs: _i(j['created_ms']),
  );

  final String id;
  final String name;
  final double duration;
  final int width;
  final int height;
  final int clips;
  final bool hasTranscript;
  final int createdMs;
}

class ViralScore {
  const ViralScore({
    required this.total,
    required this.criteria,
    required this.summary,
    required this.strengths,
    required this.improvements,
  });

  factory ViralScore.fromJson(Json j) => ViralScore(
    total: _i(j['total']),
    criteria: {for (final k in criteriaKeys) k: _i(j[k])},
    summary: j['summary'] as String? ?? '',
    strengths: (j['strengths'] as List? ?? const []).cast<String>(),
    improvements: (j['improvements'] as List? ?? const []).cast<String>(),
  );

  static const criteriaKeys = [
    'hook',
    'emotion',
    'novelty',
    'clarity',
    'pacing',
    'shareability',
    'platform_fit',
  ];

  final int total;
  final Map<String, int> criteria;
  final String summary;
  final List<String> strengths;
  final List<String> improvements;
}

class StudioClip {
  const StudioClip({
    required this.id,
    required this.title,
    required this.start,
    required this.end,
    required this.hook,
    required this.reason,
    required this.deletedWords,
    required this.removeFillers,
    required this.silence,
    required this.captions,
    required this.reframe,
    required this.brollCount,
    required this.score,
    required this.social,
    required this.template,
    required this.music,
  });

  factory StudioClip.fromJson(Json j) => StudioClip(
    id: j['id'] as String,
    title: j['title'] as String? ?? '',
    start: _d(j['start']),
    end: _d(j['end']),
    hook: j['hook'] as String?,
    reason: j['reason'] as String?,
    deletedWords: ((j['deleted_words'] as List?) ?? const [])
        .map((e) => (e as num).toInt())
        .toSet(),
    removeFillers: j['remove_fillers'] == true,
    silence: (j['silence'] as Map?)?.cast<String, dynamic>(),
    captions: (j['captions'] as Map?)?.cast<String, dynamic>(),
    reframe: (j['reframe'] as Map?)?.cast<String, dynamic>(),
    brollCount: (j['broll'] as List?)?.length ?? 0,
    score: j['score'] is Map
        ? ViralScore.fromJson((j['score'] as Map).cast())
        : null,
    social: ((j['social'] as Map?)?['posts'] as Map?)?.cast<String, dynamic>(),
    template: j['template'] as String?,
    music: (j['music'] as Map?)?.cast<String, dynamic>(),
  );

  final String id;
  final String title;
  final double start;
  final double end;
  final String? hook;
  final String? reason;
  final Set<int> deletedWords;
  final bool removeFillers;
  final Json? silence;
  final Json? captions;
  final Json? reframe;
  final int brollCount;
  final ViralScore? score;

  /// platform key -> {title, description, hashtags}
  final Json? social;
  final String? template;

  /// {path, gain, duck} background music, if any.
  final Json? music;

  double get duration => end - start;

  /// `vertical` | `square` | `custom` | null (no reframing).
  String? get aspectName {
    final r = reframe;
    if (r == null) return null;
    final a = (r['aspect'] as List?)?.map((e) => (e as num).toInt()).toList();
    if (a == null || a.length != 2) return 'custom';
    if (a[0] == 9 && a[1] == 16) return 'vertical';
    if (a[0] == a[1]) return 'square';
    return 'custom';
  }

  bool get tracked => ((reframe?['focus'] as List?) ?? const []).isNotEmpty;
}

class StudioSource {
  const StudioSource({
    required this.path,
    required this.duration,
    required this.width,
    required this.height,
    required this.fps,
    required this.hasAudio,
  });

  factory StudioSource.fromJson(Json j) => StudioSource(
    path: j['path'] as String? ?? '',
    duration: _d(j['duration']),
    width: _i(j['width']),
    height: _i(j['height']),
    fps: _d(j['fps'], 30),
    hasAudio: j['has_audio'] != false,
  );

  final String path;
  final double duration;
  final int width;
  final int height;
  final double fps;
  final bool hasAudio;
}

class StudioProjectData {
  const StudioProjectData({
    required this.id,
    required this.name,
    required this.source,
    required this.clips,
    required this.hasTranscript,
  });

  factory StudioProjectData.parse(String json) =>
      StudioProjectData.fromJson((jsonDecode(json) as Map).cast());

  factory StudioProjectData.fromJson(Json j) => StudioProjectData(
    id: j['id'] as String,
    name: j['name'] as String? ?? '',
    source: StudioSource.fromJson((j['source'] as Map).cast()),
    clips: _list(j['clips'], StudioClip.fromJson),
    hasTranscript: j['has_transcript'] == true,
  );

  final String id;
  final String name;
  final StudioSource source;
  final List<StudioClip> clips;
  final bool hasTranscript;

  StudioClip? clip(String? id) => clips.where((c) => c.id == id).firstOrNull;
}

class PreviewWord {
  const PreviewWord({
    required this.idx,
    required this.text,
    required this.start,
    required this.end,
    required this.deleted,
    required this.filler,
    required this.outStart,
    required this.outEnd,
  });

  factory PreviewWord.fromJson(Json j) => PreviewWord(
    idx: _i(j['idx']),
    text: j['text'] as String? ?? '',
    start: _d(j['start']),
    end: _d(j['end']),
    deleted: j['deleted'] == true,
    filler: j['filler'] == true,
    outStart: j['out_start'] is num ? _d(j['out_start']) : null,
    outEnd: j['out_end'] is num ? _d(j['out_end']) : null,
  );

  final int idx;
  final String text;
  final double start;
  final double end;
  final bool deleted;
  final bool filler;
  final double? outStart;
  final double? outEnd;
}

class CaptionWord {
  const CaptionWord(this.text, this.start, this.end);
  final String text;
  final double start;
  final double end;
}

class CropKey {
  const CropKey(this.t, this.x, this.y, this.w, this.h);
  final double t;
  final double x;
  final double y;
  final double w;
  final double h;
}

/// Crop window over the output timeline; mirrors `hk_media::CropPath`.
class CropPath {
  const CropPath(this.keys, {required this.smooth});

  factory CropPath.fromJson(Json j) => CropPath(
    _list(
      j['keys'],
      (k) =>
          CropKey(_d(k['t']), _d(k['x']), _d(k['y']), _d(k['w']), _d(k['h'])),
    ),
    smooth: j['interp'] == 'smooth',
  );

  final List<CropKey> keys;
  final bool smooth;

  CropKey at(double t) {
    if (keys.isEmpty) return const CropKey(0, 0, 0, 0, 0);
    if (t <= keys.first.t || keys.length == 1) return keys.first;
    if (t >= keys.last.t) return keys.last;
    for (var i = 0; i + 1 < keys.length; i++) {
      final a = keys[i];
      final b = keys[i + 1];
      if (t <= b.t) {
        var f = ((t - a.t) / (b.t - a.t)).clamp(0.0, 1.0);
        if (smooth) f = f * f * (3 - 2 * f);
        double l(double p, double q) => p + (q - p) * f;
        return CropKey(t, l(a.x, b.x), l(a.y, b.y), l(a.w, b.w), l(a.h, b.h));
      }
    }
    return keys.last;
  }
}

class BrollItem {
  const BrollItem({
    required this.start,
    required this.end,
    required this.query,
    required this.asset,
    required this.isImage,
  });

  factory BrollItem.fromJson(Json j) => BrollItem(
    start: _d(j['start']),
    end: _d(j['end']),
    query: j['query'] as String? ?? '',
    asset: j['asset'] as String? ?? '',
    isImage: j['kind'] == 'image',
  );

  final double start;
  final double end;
  final String query;
  final String asset;
  final bool isImage;

  BrollItem withAsset(String a) => BrollItem(
    start: start,
    end: end,
    query: query,
    asset: a,
    isImage: isImage,
  );
}

class PreviewPlan {
  const PreviewPlan({
    required this.clipId,
    required this.srcStart,
    required this.srcEnd,
    required this.duration,
    required this.ranges,
    required this.words,
    required this.captions,
    required this.captionStyle,
    required this.crop,
    required this.outW,
    required this.outH,
    this.broll = const [],
  });

  factory PreviewPlan.parse(String json) =>
      PreviewPlan.fromJson((jsonDecode(json) as Map).cast());

  factory PreviewPlan.fromJson(Json j) => PreviewPlan(
    clipId: j['clip_id'] as String,
    srcStart: _d(j['src_start']),
    srcEnd: _d(j['src_end']),
    duration: _d(j['duration']),
    ranges: ((j['ranges'] as List?) ?? const [])
        .map((r) => (_d((r as List)[0]), _d(r[1])))
        .toList(),
    words: _list(j['words'], PreviewWord.fromJson),
    captions: _list(
      j['captions'],
      (c) =>
          CaptionWord(c['text'] as String? ?? '', _d(c['start']), _d(c['end'])),
    ),
    captionStyle: (j['caption_style'] as Map?)?.cast<String, dynamic>(),
    crop: j['crop'] is Map
        ? CropPath.fromJson((j['crop'] as Map).cast())
        : null,
    outW: _i((j['out_size'] as List?)?[0], 1920),
    outH: _i((j['out_size'] as List?)?[1], 1080),
    broll: _list(j['broll'], BrollItem.fromJson),
  );

  PreviewPlan withBroll(List<BrollItem> items) => PreviewPlan(
    clipId: clipId,
    srcStart: srcStart,
    srcEnd: srcEnd,
    duration: duration,
    ranges: ranges,
    words: words,
    captions: captions,
    captionStyle: captionStyle,
    crop: crop,
    outW: outW,
    outH: outH,
    broll: items,
  );

  final String clipId;
  final double srcStart;
  final double srcEnd;
  final double duration;
  final List<(double, double)> ranges;
  final List<PreviewWord> words;
  final List<CaptionWord> captions;
  final Json? captionStyle;
  final CropPath? crop;
  final int outW;
  final int outH;
  final List<BrollItem> broll;

  EdlClock get clock => EdlClock(ranges);
  double get aspect => outH == 0 ? 16 / 9 : outW / outH;
}

/// Maps between the output timeline (what the viewer sees) and source time,
/// across the kept ranges played back to back.
class EdlClock {
  EdlClock(this.ranges) {
    var acc = 0.0;
    for (final r in ranges) {
      _offsets.add(acc);
      acc += r.$2 - r.$1;
    }
    duration = acc;
  }

  final List<(double, double)> ranges;
  final List<double> _offsets = [];
  late final double duration;

  /// Index of the range playing at output time [t].
  int rangeAt(double t) {
    for (var i = 0; i < ranges.length; i++) {
      if (t < _offsets[i] + (ranges[i].$2 - ranges[i].$1)) return i;
    }
    return ranges.isEmpty ? 0 : ranges.length - 1;
  }

  double toSource(double t) {
    if (ranges.isEmpty) return 0;
    final i = rangeAt(t);
    final r = ranges[i];
    return (r.$1 + (t - _offsets[i])).clamp(r.$1, r.$2);
  }

  /// Output time of source time [s]; a cut-out moment snaps to the next kept one.
  double toOutput(double s) {
    for (var i = 0; i < ranges.length; i++) {
      final r = ranges[i];
      if (s < r.$1) return _offsets[i];
      if (s <= r.$2) return _offsets[i] + (s - r.$1);
    }
    return duration;
  }

  double rangeStartOut(int i) => _offsets[i];
  double rangeEndOut(int i) => _offsets[i] + (ranges[i].$2 - ranges[i].$1);
}

class StudioEvent {
  const StudioEvent(
    this.kind, {
    this.rev,
    this.clips = const [],
    this.by,
    this.clip,
    this.fraction,
    this.path,
    this.error,
  });

  factory StudioEvent.parse(String json) {
    final j = (jsonDecode(json) as Map).cast<String, dynamic>();
    return StudioEvent(
      j['kind'] as String? ?? '',
      rev: j['rev'] is num ? _i(j['rev']) : null,
      clips: (j['clips'] as List? ?? const []).cast<String>(),
      by: j['by'] as String?,
      clip: j['clip'] as String?,
      fraction: j['fraction'] is num ? _d(j['fraction']) : null,
      path: j['path'] as String?,
      error: j['error'] as String?,
    );
  }

  final String kind;
  final int? rev;
  final List<String> clips;
  final String? by;
  final String? clip;
  final double? fraction;
  final String? path;
  final String? error;
}

String formatTime(double secs) {
  final s = secs < 0 ? 0 : secs;
  final m = s ~/ 60;
  final r = s - m * 60;
  return '$m:${r.toStringAsFixed(1).padLeft(4, '0')}';
}
