import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/studio/studio_models.dart';
import 'package:hokkabaz/screens/studio/transcript_logic.dart';

PreviewWord w(int idx, String text, double s, double e, {bool deleted = false, bool filler = false}) => PreviewWord(
  idx: idx,
  text: text,
  start: s,
  end: e,
  deleted: deleted,
  filler: filler,
  outStart: deleted ? null : s,
  outEnd: deleted ? null : e,
);

void main() {
  final words = [
    w(10, 'Hello', 0, .4),
    w(11, 'world.', .5, .9),
    w(12, 'Um', 1.0, 1.3, filler: true),
    w(13, 'second', 1.4, 1.8),
    w(14, 'one!', 1.9, 2.2),
  ];

  test('selection ordering', () {
    const s = WordSelection(3, 1);
    expect((s.lo, s.hi, s.length), (1, 3, 3));
    expect(s.contains(2), isTrue);
    expect(s.contains(4), isFalse);
    expect(const WordSelection(1, 1).extendTo(4).hi, 4);
  });

  test('delete then restore', () {
    final del = deleteOrRestoreOps('c1', words, const WordSelection(1, 2), {});
    expect(del.single['op'], 'delete_words');
    expect(del.single['words'], [11, 12]);

    final after = [words[0], w(11, 'world.', .5, .9, deleted: true), w(12, 'Um', 1.0, 1.3, deleted: true, filler: true), ...words.skip(3)];
    final res = deleteOrRestoreOps('c1', after, const WordSelection(1, 2), {11, 12});
    expect(res.single['op'], 'restore_words');
    expect(res.single['words'], [11, 12]);
    // Deleted only by the fillers switch: nothing to restore explicitly.
    expect(deleteOrRestoreOps('c1', after, const WordSelection(2, 2), {11}), isEmpty);
    // Mixed selection deletes only what is still there.
    final mixed = deleteOrRestoreOps('c1', after, const WordSelection(0, 2), {11, 12});
    expect(mixed.single['words'], [10]);
  });

  test('sentences', () {
    expect(sentenceAround(words, 0), (0, 1));
    expect(sentenceAround(words, 3), (2, 4));
    expect(sentenceAround(words, 2), (2, 4));
  });

  test('search', () {
    expect(searchWords(words, 'world'), [1]);
    expect(searchWords(words, 'hello world'), [0]);
    expect(searchWords(words, 'SECOND one'), [3]);
    expect(searchWords(words, ''), isEmpty);
    expect(searchWords(words, 'nope'), isEmpty);
  });

  test('active word by output time', () {
    expect(activeWordPos(words, 0.2), 0);
    expect(activeWordPos(words, 0.45), isNull);
    expect(activeWordPos(words, 1.5), 3);
    expect(activeWordPos(words, 5), isNull);
    final cut = [words[0], w(11, 'x', .5, .9, deleted: true), words[3]];
    expect(activeWordPos(cut, 0.7), isNull);
    expect(activeWordPos(const [], 1), isNull);
  });

  test('paragraphs split on pauses and long runs', () {
    final long = [for (var i = 0; i < 70; i++) w(i, 'w$i', i * .3, i * .3 + .2)];
    final ps = paragraphs(long);
    expect(ps.first, (0, 27));
    expect(ps.last.$2, 69);
    expect(ps.fold<int>(0, (a, p) => a + (p.$2 - p.$1 + 1)), 70);
    final gap = [w(0, 'a', 0, .2), w(1, 'b', .3, .5), w(2, 'c', 5, 5.2)];
    expect(paragraphs(gap), [(0, 1), (2, 2)]);
    expect(paragraphs(const []), isEmpty);
  });
}
