import 'dart:io';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/settings_providers.dart';

/// `@` mentions list files with plain dart:io; there is no FFI listing call.
/// Mobile sandboxes the workspace elsewhere, so it is desktop only.
bool get fileMentionsSupported => !(Platform.isAndroid || Platform.isIOS);

const _skipDirs = {
  '.git',
  'node_modules',
  'target',
  'build',
  '.dart_tool',
  '.idea',
  '.venv',
};

/// Workspace-relative file paths (`/` separated), breadth first so shallow
/// files survive the [max] cap in big trees.
Future<List<String>> listWorkspaceFiles(
  String root, {
  int max = 4000,
  int maxDepth = 8,
}) async {
  final out = <String>[];
  final base = Directory(root);
  if (!await base.exists()) return out;
  var level = <Directory>[base];
  for (var depth = 0; depth <= maxDepth && level.isNotEmpty; depth++) {
    final next = <Directory>[];
    for (final dir in level) {
      try {
        await for (final e in dir.list(followLinks: false)) {
          final name = e.uri.pathSegments.where((s) => s.isNotEmpty).last;
          if (e is Directory) {
            if (!_skipDirs.contains(name)) next.add(e);
          } else if (e is File) {
            var rel = e.path.substring(root.length);
            rel = rel.replaceAll(Platform.pathSeparator, '/');
            out.add(rel.startsWith('/') ? rel.substring(1) : rel);
            if (out.length >= max) return out;
          }
        }
      } on FileSystemException {
        // Unreadable dir: skip it.
      }
    }
    level = next;
  }
  return out;
}

/// Re-listed each time the `@` menu opens (autoDispose drops it after).
final workspaceFilesProvider = FutureProvider.autoDispose<List<String>>((
  ref,
) async {
  if (!fileMentionsSupported) return const [];
  final root = ref.watch(
    settingsProvider.select((s) => s.value?.workspaceRoot),
  );
  if (root == null) return const [];
  return listWorkspaceFiles(root);
});
