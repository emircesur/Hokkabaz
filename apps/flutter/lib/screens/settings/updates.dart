import 'dart:convert';
import 'dart:io';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/version.dart';

class ReleaseInfo {
  const ReleaseInfo({
    required this.tag,
    required this.url,
    required this.notes,
  });
  final String tag, url, notes;
}

/// `1.2.3` / `v1.2.3` / `1.2.3-rc1` -> [1,2,3]; null when it has no numeric core.
List<int>? parseVersion(String v) {
  final core = v
      .trim()
      .replaceFirst(RegExp('^[vV]'), '')
      .split(RegExp(r'[-+]'))
      .first;
  final parts = core.split('.');
  if (parts.isEmpty || parts.length > 4) return null;
  final nums = <int>[];
  for (final p in parts) {
    final n = int.tryParse(p);
    if (n == null) return null;
    nums.add(n);
  }
  return nums;
}

bool isNewer(String candidate, String current) {
  final a = parseVersion(candidate), b = parseVersion(current);
  if (a == null || b == null) return false;
  for (var i = 0; i < 4; i++) {
    final x = i < a.length ? a[i] : 0, y = i < b.length ? b[i] : 0;
    if (x != y) return x > y;
  }
  return false;
}

/// Latest GitHub release if it is newer than this build; null when up to date.
/// Throws on network/format errors so the UI can say the check failed.
Future<ReleaseInfo?> checkForUpdate({HttpClient? client}) async {
  final c = client ?? HttpClient()
    ..connectionTimeout = const Duration(seconds: 10);
  try {
    final req = await c.getUrl(
      Uri.parse('https://api.github.com/repos/$hkRepo/releases/latest'),
    );
    req.headers.set('Accept', 'application/vnd.github+json');
    req.headers.set('User-Agent', 'hokkabaz/$hkVersion');
    final res = await req.close().timeout(const Duration(seconds: 15));
    if (res.statusCode == 404) return null; // no releases yet
    if (res.statusCode != 200) {
      throw HttpException('GitHub answered ${res.statusCode}');
    }
    final body = await res.transform(utf8.decoder).join();
    final j = jsonDecode(body) as Map<String, dynamic>;
    final tag = (j['tag_name'] as String?) ?? '';
    if (!isNewer(tag, hkVersion)) return null;
    return ReleaseInfo(
      tag: tag,
      url:
          (j['html_url'] as String?) ??
          'https://github.com/$hkRepo/releases/latest',
      notes: ((j['body'] as String?) ?? '').trim(),
    );
  } finally {
    if (client == null) c.close();
  }
}

/// Manual check from Settings; nothing is downloaded or installed automatically.
class UpdateCheck extends AsyncNotifier<ReleaseInfo?> {
  bool checked = false;

  @override
  Future<ReleaseInfo?> build() async => null;

  Future<void> run() async {
    state = const AsyncLoading();
    state = await AsyncValue.guard(checkForUpdate);
    checked = true;
  }
}

final updateCheckProvider = AsyncNotifierProvider<UpdateCheck, ReleaseInfo?>(
  UpdateCheck.new,
);
