import 'dart:io';

import 'package:flutter_local_notifications/flutter_local_notifications.dart';

import '../ffi/notifications.dart';
import 'activity.dart';

/// System notifications for long jobs (P9.7): an ongoing progress notification with a Cancel
/// action on Android, and a "done / failed" toast on every platform.
class JobNotifications {
  JobNotifications._();
  static final instance = JobNotifications._();

  final _p = FlutterLocalNotificationsPlugin();
  bool _ready = false;
  final Set<int> _shown = {};
  final Map<int, int> _lastPct = {};

  /// Set by the shell: cancels a job when its notification's Cancel action is tapped.
  void Function(int jobId)? onCancel;

  Future<void> init() async {
    if (_ready) return;
    try {
      await _p.initialize(
        settings: const InitializationSettings(
          android: AndroidInitializationSettings('@mipmap/ic_launcher'),
          linux: LinuxInitializationSettings(defaultActionName: 'Open'),
          macOS: DarwinInitializationSettings(),
          windows: WindowsInitializationSettings(
            appName: 'Hokkabaz',
            appUserModelId: 'Dev.Hokkabaz.Hokkabaz',
            guid: '6f1c2a7e-4b8d-4e0a-9d3c-2b5f7a1e9c40',
          ),
        ),
        onDidReceiveNotificationResponse: (r) {
          final id = int.tryParse(r.payload ?? '');
          if (r.actionId == 'cancel' && id != null) onCancel?.call(id);
        },
      );
      if (Platform.isAndroid) {
        await _p
            .resolvePlatformSpecificImplementation<
              AndroidFlutterLocalNotificationsPlugin
            >()
            ?.requestNotificationsPermission();
      }
      _ready = true;
    } catch (_) {
      // No notification daemon (headless Linux, CI): in-app snackbars still cover it.
    }
  }

  /// Ongoing progress (Android only; desktop shows the Activity panel instead).
  Future<void> progress(Job j) async {
    if (!_ready || !Platform.isAndroid) return;
    final pct = ((j.progress ?? 0) * 100).round();
    if (_shown.contains(j.id) && _lastPct[j.id] == pct) return;
    _shown.add(j.id);
    _lastPct[j.id] = pct;
    await _p.show(
      id: j.id,
      title: j.title,
      body: j.phase ?? 'Working…',
      payload: '${j.id}',
      notificationDetails: NotificationDetails(
        android: AndroidNotificationDetails(
          'jobs',
          'Running jobs',
          channelDescription: 'Progress of long-running work',
          importance: Importance.low,
          priority: Priority.low,
          ongoing: true,
          onlyAlertOnce: true,
          showProgress: true,
          maxProgress: 100,
          progress: pct,
          indeterminate: j.progress == null,
          actions: const [
            AndroidNotificationAction(
              'cancel',
              'Cancel',
              cancelNotification: false,
            ),
          ],
        ),
      ),
    );
  }

  Future<void> finished(Job j, String message) async {
    if (!_ready) return;
    _lastPct.remove(j.id);
    if (_shown.remove(j.id)) await _p.cancel(id: j.id);
    await _p.show(
      id: j.id + 1000000,
      title: message,
      body: j.phase,
      notificationDetails: const NotificationDetails(
        android: AndroidNotificationDetails(
          'jobs_done',
          'Finished jobs',
          channelDescription: 'Long-running work finished or failed',
        ),
        linux: LinuxNotificationDetails(),
        windows: WindowsNotificationDetails(),
      ),
    );
  }
}

/// Desktop approval requests as OS notifications (Android has its own channel-based notifier).
class DesktopApprovalNotifier implements ApprovalNotifier {
  const DesktopApprovalNotifier();

  int _id(String approvalId) => 2000000 + (approvalId.hashCode & 0xfffff);

  @override
  Future<void> show(ApprovalNotice n) async {
    final j = JobNotifications.instance;
    if (!j._ready) return;
    await j._p.show(
      id: _id(n.approvalId),
      title: 'Approval needed: ${n.tool}',
      body: n.detail,
      notificationDetails: const NotificationDetails(
        linux: LinuxNotificationDetails(),
        windows: WindowsNotificationDetails(),
      ),
    );
  }

  @override
  Future<void> cancel(String approvalId) async {
    final j = JobNotifications.instance;
    if (j._ready) await j._p.cancel(id: _id(approvalId));
  }
}
