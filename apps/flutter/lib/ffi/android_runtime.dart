import 'dart:io' show Platform;

import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'notifications.dart';

const _channel = MethodChannel('hokkabaz/runtime');

bool get _android => Platform.isAndroid;

Future<void> _call(String method, [Map<String, Object?>? args]) async {
  if (!_android) return;
  try {
    await _channel.invokeMethod<void>(method, args);
  } on PlatformException {
    // Notifications/service are best effort; the app works without them.
  } on MissingPluginException {
    // Not running inside the Android host (tests).
  }
}

/// Asks for the notification permission (Android 13+); harmless elsewhere.
Future<void> requestNotificationPermission() => _call('requestNotifications');

class BatteryInfo {
  const BatteryInfo(this.level, this.charging, this.powerSave);
  final int level;
  final bool charging;
  final bool powerSave;

  /// Background work is likely to be throttled or the user is short on charge.
  bool get constrained => powerSave || (!charging && level >= 0 && level < 15);
}

Future<BatteryInfo?> readBattery() async {
  if (!_android) return null;
  try {
    final m = await _channel.invokeMapMethod<String, Object?>('battery');
    if (m == null) return null;
    return BatteryInfo(
      (m['level'] as num?)?.toInt() ?? -1,
      m['charging'] == true,
      m['powerSave'] == true,
    );
  } on Object {
    return null;
  }
}

/// Approval requests as Android notifications, so a turn waiting on the user is visible with
/// the app in the background.
class AndroidApprovalNotifier implements ApprovalNotifier {
  const AndroidApprovalNotifier();

  int _id(String approvalId) => approvalId.hashCode & 0x7fffffff;

  @override
  Future<void> show(ApprovalNotice n) => _call('notify', {
    'id': _id(n.approvalId),
    'title': 'Approval needed: ${n.tool}',
    'text': n.detail ?? 'The agent is waiting for your decision.',
  });

  @override
  Future<void> cancel(String approvalId) => _call('cancel', {'id': _id(approvalId)});
}

/// Things that must not be frozen right now (agent turns, flow runs), by key.
class ActiveWork extends Notifier<Set<String>> {
  @override
  Set<String> build() => const {};

  void set(String key, bool active) {
    if (state.contains(key) == active) return;
    state = active ? {...state, key} : ({...state}..remove(key));
  }
}

final activeWorkProvider = NotifierProvider<ActiveWork, Set<String>>(ActiveWork.new);

/// Hint shown when background work is likely to be throttled.
class KeepAlive {
  const KeepAlive({this.running = false, this.hint});
  final bool running;
  final String? hint;
}

/// Runs the Android foreground service while [activeWorkProvider] is non-empty. Under battery
/// saver or a low battery the service is skipped (the OS would throttle it anyway) and the user
/// is told that work may pause when the screen turns off.
class KeepAliveNotifier extends Notifier<KeepAlive> {
  bool _started = false;

  @override
  KeepAlive build() {
    ref.listen(activeWorkProvider, (_, work) => _sync(work.length));
    ref.onDispose(() {
      if (_started) _call('stop');
    });
    return const KeepAlive();
  }

  Future<void> _sync(int n) async {
    if (!_android) return;
    if (n > 0 && !_started) {
      final b = await readBattery();
      if (b != null && b.constrained) {
        state = const KeepAlive(
          hint: 'Battery saver or low battery: background runs may pause when the screen turns off.',
        );
        return;
      }
      _started = true;
      state = const KeepAlive(running: true);
      await _call('start', {'text': 'Running $n task${n == 1 ? '' : 's'} in the background'});
    } else if (n == 0 && _started) {
      _started = false;
      state = const KeepAlive();
      await _call('stop');
    } else if (n == 0) {
      state = const KeepAlive();
    }
  }
}

final keepAliveProvider = NotifierProvider<KeepAliveNotifier, KeepAlive>(KeepAliveNotifier.new);
