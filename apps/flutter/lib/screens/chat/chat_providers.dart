import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../ffi/notifications.dart';
import '../../src/rust/api/chat.dart' as chat;
import '../../src/rust/api/settings.dart' as settings;
import '../../src/rust/dto.dart';
import '../../src/rust/error.dart';
import 'chat_model.dart';

/// User-facing text for an FFI failure.
String describeError(Object e) => e is HkError ? e.message : e.toString();

/// Currently open session; null = list (mobile) / empty state (desktop).
class SelectedSession extends Notifier<String?> {
  @override
  String? build() => null;

  void select(String? id) => state = id;
}

final selectedSessionProvider = NotifierProvider<SelectedSession, String?>(
  SelectedSession.new,
);

/// Session list, newest first. Refreshed after create / title change / turn end.
class SessionList extends AsyncNotifier<List<SessionInfoDto>> {
  @override
  Future<List<SessionInfoDto>> build() async {
    if (ref.watch(backendErrorProvider) != null) return const [];
    return _fetch();
  }

  Future<List<SessionInfoDto>> _fetch() async {
    final list = await chat.listSessions(limit: 200, offset: 0);
    return [...list]..sort((a, b) => b.updatedMs.compareTo(a.updatedMs));
  }

  Future<void> refresh() async {
    if (ref.read(backendErrorProvider) != null) return;
    state = await AsyncValue.guard(_fetch);
  }

  /// Creates and selects a session. Returns the error text on failure.
  Future<String?> create() async {
    try {
      final s = await chat.createSession();
      ref.read(selectedSessionProvider.notifier).select(s.id);
      await refresh();
      return null;
    } catch (e) {
      return describeError(e);
    }
  }
}

final sessionListProvider =
    AsyncNotifierProvider<SessionList, List<SessionInfoDto>>(SessionList.new);

/// plan/build toggle for new turns. Seeded from stored preferences.
class ChatMode extends Notifier<ModeDto> {
  @override
  ModeDto build() {
    if (ref.watch(backendErrorProvider) == null) {
      unawaited(_seed());
    }
    return ModeDto.build;
  }

  Future<void> _seed() async {
    try {
      final s = await settings.getSettings();
      state = s.mode;
    } catch (_) {
      // Keep the default; the toggle still works.
    }
  }

  void set(ModeDto m) => state = m;
}

final chatModeProvider = NotifierProvider<ChatMode, ModeDto>(ChatMode.new);

/// Model override for new turns (`provider/model`); null = session default.
class ComposerModel extends Notifier<String?> {
  @override
  String? build() => null;

  void set(String? reference) => state = reference;
}

final composerModelProvider = NotifierProvider<ComposerModel, String?>(
  ComposerModel.new,
);

/// One open conversation: history + live feed + actions.
class ChatController extends Notifier<ChatState> {
  ChatController(this.sessionId);

  final String sessionId;
  StreamSubscription<SessionEventDto>? _sub;

  /// Tools the user chose to always allow in this session (client-side rule: the core only
  /// knows one-shot decisions, so matching requests are answered here, instantly).
  final Set<String> _alwaysAllow = {};
  final Set<String> _autoAnswered = {};

  Set<String> get alwaysAllowed => Set.unmodifiable(_alwaysAllow);

  void allowAlways(String tool) {
    _alwaysAllow.add(tool);
    _autoAnswer();
  }

  void revokeAlways(String tool) => _alwaysAllow.remove(tool);

  void _autoAnswer() {
    for (final t in state.pendingApprovals) {
      final id = t.approval!.id;
      if (_alwaysAllow.contains(t.name) && _autoAnswered.add(id)) {
        unawaited(respond(id, approved: true, reason: 'always allowed in this session'));
      }
    }
  }

  /// Live events that arrive while history is still loading; replayed after.
  final List<SessionEventDto> _early = [];
  bool _historyReady = false;
  bool _disposed = false;

  @override
  ChatState build() {
    ref.onDispose(() {
      _disposed = true;
      _sub?.cancel();
    });
    _start();
    return const ChatState();
  }

  void _start() {
    // Subscribe before loading history so no event falls in the gap.
    _sub = chat
        .sessionEvents(sessionId: sessionId)
        .listen(_onEvent, onError: (Object e) => _notice(describeError(e)));
    unawaited(_loadHistory());
  }

  Future<void> _loadHistory() async {
    try {
      final detail = await chat.getSession(sessionId: sessionId);
      if (_disposed) return;
      var s = chatStateFromDetail(detail);
      for (final e in _early) {
        s = reduceEvent(s, e);
      }
      _early.clear();
      _historyReady = true;
      state = s;
      _syncApprovals();
    } catch (e) {
      if (_disposed) return;
      _historyReady = true;
      state = state.copyWith(loading: false);
      _notice(describeError(e));
    }
  }

  void _onEvent(SessionEventDto e) {
    if (!_historyReady) {
      _early.add(e);
      return;
    }
    state = reduceEvent(state, e);
    _autoAnswer();
    _syncApprovals();
    if (e.kind == SessionEventKind.titleChanged ||
        e.kind == SessionEventKind.turnEnded) {
      unawaited(ref.read(sessionListProvider.notifier).refresh());
    }
  }

  /// Publishes pending approvals so the shell can badge / banner them even
  /// when another tab is open.
  void _syncApprovals() {
    if (_disposed) return;
    ref.read(approvalInboxProvider.notifier).sync(sessionId, [
      for (final t in state.pendingApprovals)
        ApprovalNotice(
          sessionId: sessionId,
          approvalId: t.approval!.id,
          tool: t.name,
          detail: t.approval!.detail,
        ),
    ]);
  }

  void _notice(String text) {
    if (_disposed) return;
    state = state.copyWith(
      items: [
        ...state.items,
        NoticeItem(text, kind: NoticeKind.error),
      ],
    );
  }

  Future<void> send(String prompt, ModeDto mode, {String? model}) async {
    final text = prompt.trim();
    if (text.isEmpty) return;
    try {
      // While a turn runs, a plain prompt would only queue; steer it instead.
      if (state.running) {
        await chat.inject(sessionId: sessionId, text: text);
      } else {
        await chat.submitTurn(
          sessionId: sessionId,
          prompt: text,
          mode: mode,
          model: model,
        );
      }
    } catch (e) {
      _notice(describeError(e));
    }
  }

  Future<void> cancel() async {
    try {
      await chat.cancel(sessionId: sessionId);
    } catch (e) {
      _notice(describeError(e));
    }
  }

  Future<void> respond(
    String approvalId, {
    required bool approved,
    String? reason,
  }) async {
    try {
      await chat.respondApproval(
        sessionId: sessionId,
        approvalId: approvalId,
        approved: approved,
        reason: (reason == null || reason.trim().isEmpty)
            ? null
            : reason.trim(),
      );
    } catch (e) {
      _notice(describeError(e));
    }
  }
}

final chatControllerProvider = NotifierProvider.autoDispose
    .family<ChatController, ChatState, String>(ChatController.new);
