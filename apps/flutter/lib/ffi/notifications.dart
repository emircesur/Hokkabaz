import 'package:flutter_riverpod/flutter_riverpod.dart';

/// One tool call waiting on the user.
class ApprovalNotice {
  const ApprovalNotice({
    required this.sessionId,
    required this.approvalId,
    required this.tool,
    this.detail,
  });

  final String sessionId;
  final String approvalId;
  final String tool;
  final String? detail;
}

/// OS-level notification sink for approval requests (DESIGN §4.3: mobile
/// approvals arrive as notifications).
///
/// The default is a no-op: no native notification plugin is a dependency yet
/// (it needs per-platform build config). The in-app banner + nav badge driven
/// by [ApprovalInbox] cover the foreground case; a plugin-backed
/// implementation overrides [approvalNotifierProvider] later.
abstract interface class ApprovalNotifier {
  Future<void> show(ApprovalNotice notice);

  /// The approval was answered or its turn ended; drop any posted notification.
  Future<void> cancel(String approvalId);
}

class NoopApprovalNotifier implements ApprovalNotifier {
  const NoopApprovalNotifier();

  @override
  Future<void> show(ApprovalNotice notice) async {}

  @override
  Future<void> cancel(String approvalId) async {}
}

final approvalNotifierProvider = Provider<ApprovalNotifier>(
  (ref) => const NoopApprovalNotifier(),
);

/// Pending approvals across sessions, keyed by session id. Chat controllers
/// push their current set via [sync]; the shell reads it for badge + banner.
class ApprovalInbox extends Notifier<Map<String, List<ApprovalNotice>>> {
  @override
  Map<String, List<ApprovalNotice>> build() => const {};

  List<ApprovalNotice> get all => [for (final l in state.values) ...l];

  void sync(String sessionId, List<ApprovalNotice> pending) {
    final before = state[sessionId] ?? const <ApprovalNotice>[];
    final beforeIds = {for (final n in before) n.approvalId};
    final afterIds = {for (final n in pending) n.approvalId};
    if (beforeIds.length == afterIds.length &&
        beforeIds.containsAll(afterIds)) {
      return;
    }
    final next = {...state};
    if (pending.isEmpty) {
      next.remove(sessionId);
    } else {
      next[sessionId] = pending;
    }
    state = next;
    final sink = ref.read(approvalNotifierProvider);
    for (final n in pending) {
      if (!beforeIds.contains(n.approvalId)) sink.show(n);
    }
    for (final n in before) {
      if (!afterIds.contains(n.approvalId)) sink.cancel(n.approvalId);
    }
  }
}

final approvalInboxProvider =
    NotifierProvider<ApprovalInbox, Map<String, List<ApprovalNotice>>>(
      ApprovalInbox.new,
    );
