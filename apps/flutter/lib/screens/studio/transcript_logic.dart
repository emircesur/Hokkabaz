import 'studio_models.dart';

/// Selected stretch of words, as positions in the clip's word list.
class WordSelection {
  const WordSelection(this.anchor, this.focus);

  final int anchor;
  final int focus;

  int get lo => anchor < focus ? anchor : focus;
  int get hi => anchor < focus ? focus : anchor;
  bool contains(int pos) => pos >= lo && pos <= hi;
  int get length => hi - lo + 1;

  WordSelection extendTo(int pos) => WordSelection(anchor, pos);
}

/// EditOp maps for deleting the selection, or restoring it when it is already
/// all deleted. Words removed only by the "remove fillers" switch are not part
/// of the clip's explicit list, so they are left to that switch.
List<Map<String, dynamic>> deleteOrRestoreOps(
  String clipId,
  List<PreviewWord> words,
  WordSelection sel,
  Set<int> explicitlyDeleted,
) {
  final picked = <PreviewWord>[
    for (var i = sel.lo; i <= sel.hi && i < words.length; i++) words[i],
  ];
  if (picked.isEmpty) return const [];
  final toDelete = picked.where((w) => !w.deleted).map((w) => w.idx).toList();
  if (toDelete.isNotEmpty) {
    return [
      {'op': 'delete_words', 'clip': clipId, 'words': toDelete},
    ];
  }
  final toRestore = picked
      .where((w) => explicitlyDeleted.contains(w.idx))
      .map((w) => w.idx)
      .toList();
  if (toRestore.isEmpty) return const [];
  return [
    {'op': 'restore_words', 'clip': clipId, 'words': toRestore},
  ];
}

/// Start / end (inclusive) positions of the sentence around [pos].
(int, int) sentenceAround(List<PreviewWord> words, int pos) {
  bool ends(String t) => t.endsWith('.') || t.endsWith('!') || t.endsWith('?');
  var lo = pos;
  while (lo > 0 && !ends(words[lo - 1].text)) {
    lo--;
  }
  var hi = pos;
  while (hi < words.length - 1 && !ends(words[hi].text)) {
    hi++;
  }
  return (lo, hi);
}

/// Positions of words whose text contains [query] (case-insensitive, whole phrase across words).
List<int> searchWords(List<PreviewWord> words, String query) {
  final q = query.trim().toLowerCase();
  if (q.isEmpty) return const [];
  final parts = q.split(RegExp(r'\s+'));
  final out = <int>[];
  for (var i = 0; i + parts.length <= words.length; i++) {
    var ok = true;
    for (var k = 0; k < parts.length; k++) {
      final w = words[i + k].text.toLowerCase().replaceAll(
        RegExp(r'[^\p{L}\p{N}]', unicode: true),
        '',
      );
      final p = parts[k].replaceAll(
        RegExp(r'[^\p{L}\p{N}]', unicode: true),
        '',
      );
      if (!w.contains(p)) {
        ok = false;
        break;
      }
    }
    if (ok) out.add(i);
  }
  return out;
}

/// Word being spoken at output time [t], by index in [words]; null in a cut or a gap.
int? activeWordPos(List<PreviewWord> words, double t) {
  var lo = 0;
  var hi = words.length - 1;
  // Words are in source order and kept words are monotonic in output time; scan neighbours of a
  // binary search over kept words only.
  final kept = <int>[
    for (var i = 0; i < words.length; i++)
      if (words[i].outStart != null) i,
  ];
  if (kept.isEmpty) return null;
  lo = 0;
  hi = kept.length - 1;
  while (lo <= hi) {
    final mid = (lo + hi) >> 1;
    final w = words[kept[mid]];
    if (t < w.outStart!) {
      hi = mid - 1;
    } else if (t >= w.outEnd!) {
      lo = mid + 1;
    } else {
      return kept[mid];
    }
  }
  return null;
}

/// Paragraph breaks: a long pause, a sentence end after enough words, or too many words.
List<(int, int)> paragraphs(
  List<PreviewWord> words, {
  double gap = 1.2,
  int maxWords = 28,
}) {
  final out = <(int, int)>[];
  var start = 0;
  for (var i = 0; i < words.length; i++) {
    final last = i == words.length - 1;
    final nextGap = last ? 0.0 : words[i + 1].start - words[i].end;
    final count = i - start + 1;
    final sentenceEnd =
        words[i].text.endsWith('.') ||
        words[i].text.endsWith('!') ||
        words[i].text.endsWith('?');
    if (last ||
        nextGap > gap ||
        count >= maxWords ||
        (sentenceEnd && count >= 12)) {
      out.add((start, i));
      start = i + 1;
    }
  }
  return out;
}
