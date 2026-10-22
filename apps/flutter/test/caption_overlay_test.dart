import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/studio/caption_overlay.dart';
import 'package:hokkabaz/screens/studio/studio_models.dart';

void main() {
  const words = [
    CaptionWord('one', 0.0, 0.4),
    CaptionWord('two.', 0.5, 0.9),
    CaptionWord('three', 1.0, 1.4),
    CaptionWord('late', 4.0, 4.4),
  ];

  test('groups on punctuation and pauses', () {
    final lines = groupCaptionLines(words, null);
    expect(lines.map((l) => l.length).toList(), [2, 1, 1]);
    final tight = groupCaptionLines(words, {'max_words': 1});
    expect(tight.length, 4);
  });

  test('finds the line and active word', () {
    final lines = groupCaptionLines(words, null);
    expect(captionAt(lines, 0.2)!.active, 0);
    expect(captionAt(lines, 0.6)!.active, 1);
    expect(captionAt(lines, 0.95)!.line.length, 2, reason: 'lingers briefly after the line');
    expect(captionAt(lines, 2.5), isNull);
    expect(captionAt(lines, 4.1)!.line.single.text, 'late');
  });

  test('hex colors', () {
    expect(hexColor('#FFE600', Colors.red), const Color(0xFFFFE600));
    expect(hexColor('nope', Colors.red), Colors.red);
    expect(hexColor(null, Colors.blue), Colors.blue);
  });

  testWidgets('overlay paints the active line', (tester) async {
    final t = ValueNotifier<double>(0.2);
    await tester.pumpWidget(
      MaterialApp(
        home: SizedBox(
          width: 400,
          height: 700,
          child: CaptionOverlay(lines: groupCaptionLines(words, null), style: const {'mode': 'karaoke', 'size': 72, 'uppercase': true, 'pos': 'center', 'highlight': '#FFE600'}, time: t),
        ),
      ),
    );
    expect(find.textContaining('ONE', findRichText: true), findsOneWidget);
    t.value = 3.0;
    await tester.pump();
    expect(find.textContaining('ONE', findRichText: true), findsNothing);
    t.value = 4.1;
    await tester.pump();
    expect(find.textContaining('LATE', findRichText: true), findsOneWidget);
  });
}
