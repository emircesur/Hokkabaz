import 'dart:typed_data';

/// Pure helpers for the composer menus. No Flutter, no FFI.

class SlashCommand {
  const SlashCommand(this.name, this.help);
  final String name;
  final String help;
}

const slashCommands = <SlashCommand>[
  SlashCommand('plan', 'Switch to plan mode'),
  SlashCommand('build', 'Switch to build mode'),
  SlashCommand('model', 'Pick the model for new turns'),
  SlashCommand('clear', 'Start a fresh session'),
];

/// Text is a bare `/word` (no whitespace yet): returns the typed query.
String? slashQuery(String text) {
  if (!text.startsWith('/')) return null;
  final q = text.substring(1);
  if (q.contains(RegExp(r'\s'))) return null;
  return q;
}

List<SlashCommand> filterSlash(String query) {
  final q = query.toLowerCase();
  return [
    for (final c in slashCommands)
      if (c.name.startsWith(q)) c,
  ];
}

/// `@token` ending at [cursor]: [start] is the index of `@`.
class MentionToken {
  const MentionToken(this.start, this.query);
  final int start;
  final String query;
}

MentionToken? detectMention(String text, int cursor) {
  if (cursor < 0 || cursor > text.length) return null;
  var i = cursor - 1;
  while (i >= 0) {
    final c = text[i];
    if (c == '@') {
      if (i == 0 || RegExp(r'\s').hasMatch(text[i - 1])) {
        return MentionToken(i, text.substring(i + 1, cursor));
      }
      return null;
    }
    if (RegExp(r'\s').hasMatch(c)) return null;
    i--;
  }
  return null;
}

/// Replaces the `@query` span with `@path ` and returns text + new cursor.
({String text, int cursor}) applyMention(
  String text,
  MentionToken token,
  int cursor,
  String path,
) {
  final insert = '@$path ';
  final out = text.replaceRange(token.start, cursor, insert);
  return (text: out, cursor: token.start + insert.length);
}

/// Subsequence match score; null = no match. Rewards consecutive runs,
/// word starts and hits in the file name over the directory part.
double? fuzzyScore(String query, String candidate) {
  if (query.isEmpty) return 0;
  final q = query.toLowerCase();
  final c = candidate.toLowerCase();
  final nameStart = c.lastIndexOf('/') + 1;
  var qi = 0;
  var score = 0.0;
  var prev = -2;
  for (var i = 0; i < c.length && qi < q.length; i++) {
    if (c[i] != q[qi]) continue;
    score += 1;
    if (prev == i - 1) score += 3;
    if (i == 0 || '/._-'.contains(c[i - 1])) score += 2;
    if (i >= nameStart) score += 1;
    prev = i;
    qi++;
  }
  if (qi < q.length) return null;
  return score - c.length * 0.01;
}

List<String> fuzzyFilter(String query, List<String> items, {int limit = 8}) {
  if (query.isEmpty) {
    // Shallow paths first: usually what you want without typing anything.
    final sorted = [...items]
      ..sort((a, b) {
        final d = '/'.allMatches(a).length - '/'.allMatches(b).length;
        return d != 0 ? d : a.compareTo(b);
      });
    return sorted.take(limit).toList();
  }
  final scored = <(String, double)>[];
  for (final it in items) {
    final s = fuzzyScore(query, it);
    if (s != null) scored.add((it, s));
  }
  scored.sort((a, b) => b.$2.compareTo(a.$2));
  return [for (final s in scored.take(limit)) s.$1];
}

/// Flutter's clipboard API only carries text, so image paste is unavailable
/// without a native plugin. The composer hides the attach UI while false.
const imagePasteAvailable = false;

Future<Uint8List?> readClipboardImage() async => null;
