import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import '../../src/rust/api/studio.dart' as api;

/// True when the Studio runs on a connected instance rather than on this device.
bool get studioRemote {
  try {
    return api.studioIsRemote();
  } catch (_) {
    return false;
  }
}

/// Something a player can open for [serverPath]: the path itself locally, a short-lived URL
/// for a connected instance.
Future<String> studioMediaFor(String serverPath) => studioRemote ? api.studioMedia(path: serverPath) : Future.value(serverPath);

/// Copies [localPath] to where the Studio runs (nothing to do when that is this device).
/// [onProgress] gets 0..1.
Future<String> studioUploadFor(String localPath, {void Function(double)? onProgress}) async {
  if (!studioRemote) return localPath;
  String? out;
  await for (final ev in api.studioUpload(localPath: localPath)) {
    final j = (jsonDecode(ev) as Map).cast<String, dynamic>();
    if (j['path'] is String) {
      out = j['path'] as String;
    } else if (j['sent'] is num && j['total'] is num) {
      final total = (j['total'] as num).toDouble();
      onProgress?.call(total <= 0 ? 1 : (j['sent'] as num) / total);
    }
  }
  return out ?? (throw StateError('The upload did not finish'));
}

/// Runs [work] behind a blocking progress dialog (uploads can take minutes).
Future<T> withUploadDialog<T>(BuildContext context, String title, Future<T> Function(void Function(double) progress) work) async {
  final value = ValueNotifier<double?>(null);
  final nav = Navigator.of(context, rootNavigator: true);
  unawaited(showDialog<void>(
    context: context,
    barrierDismissible: false,
    builder: (_) => PopScope(
      canPop: false,
      child: AlertDialog(
        title: Text(title),
        content: ValueListenableBuilder<double?>(
          valueListenable: value,
          builder: (_, v, _) => Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              LinearProgressIndicator(value: v),
              const SizedBox(height: 8),
              Text(v == null ? 'Starting…' : '${(v * 100).round()}%'),
            ],
          ),
        ),
      ),
    ),
  ));
  try {
    return await work((p) => value.value = p);
  } finally {
    if (nav.canPop()) nav.pop();
    value.dispose();
  }
}

/// Save a rendered clip that lives on the instance: download it, then let the user pick where
/// it goes. Returns the saved location, or null when cancelled.
Future<String?> saveRemoteClip(String serverPath, String suggestedName) async {
  final tmp = await getTemporaryDirectory();
  final dest = '${tmp.path}${Platform.pathSeparator}$suggestedName';
  await api.studioDownload(serverPath: serverPath, dest: dest);
  final bytes = await File(dest).readAsBytes();
  try {
    return (await FilePicker.saveFile(dialogTitle: 'Save clip', fileName: suggestedName, bytes: bytes))?.toString();
  } finally {
    unawaitedDelete(dest);
  }
}

void unawaitedDelete(String path) {
  File(path).delete().then((_) {}, onError: (_) {});
}

/// Low-res proxy of the source, made on the instance on first use.
Future<String> studioProxyFor(String projectId) => api.studioProxy(id: projectId);
