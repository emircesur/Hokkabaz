import 'dart:convert';

import 'package:flutter/foundation.dart';

import '../../src/rust/dto.dart';

/// Pure chat view-model: folds the Rust session feed (and history) into a list
/// of renderable items. No Flutter widgets, no FFI calls, so it stays testable.

enum ToolStatus { streaming, running, awaitingApproval, done, error, denied }

@immutable
class ApprovalState {
  const ApprovalState({
    required this.id,
    this.detail,
    this.resolved = false,
    this.approved = false,
    this.reason,
  });

  final String id;
  final String? detail;
  final bool resolved;
  final bool approved;
  final String? reason;

  ApprovalState resolve({required bool approved, String? reason}) =>
      ApprovalState(
        id: id,
        detail: detail,
        resolved: true,
        approved: approved,
        reason: reason,
      );
}

@immutable
sealed class ChatItem {
  const ChatItem();
}

class UserItem extends ChatItem {
  const UserItem(this.text);
  final String text;
}

class AssistantItem extends ChatItem {
  const AssistantItem({
    this.text = '',
    this.reasoning = '',
    this.streaming = true,
    this.reasoningTokens,
  });

  final String text;
  final String reasoning;

  /// Still receiving deltas; `assistantMessage` flips it off.
  final bool streaming;

  /// Provider-reported count; null until the step's usage event lands.
  final int? reasoningTokens;

  AssistantItem copyWith({
    String? text,
    String? reasoning,
    bool? streaming,
    int? reasoningTokens,
  }) => AssistantItem(
    text: text ?? this.text,
    reasoning: reasoning ?? this.reasoning,
    streaming: streaming ?? this.streaming,
    reasoningTokens: reasoningTokens ?? this.reasoningTokens,
  );

  /// Reasoning size label: reported tokens, else a rough chars/4 estimate.
  String get reasoningLabel {
    final n = reasoningTokens;
    if (n != null && n > 0) return '$n tokens';
    return '~${(reasoning.length / 4).ceil()} tokens';
  }
}

class ToolItem extends ChatItem {
  const ToolItem({
    required this.callId,
    required this.name,
    this.argsJson = '',
    this.status = ToolStatus.streaming,
    this.result,
    this.approval,
  });

  final String callId;
  final String name;

  /// Raw JSON text; partial while [status] is [ToolStatus.streaming].
  final String argsJson;
  final ToolStatus status;
  final String? result;
  final ApprovalState? approval;

  ToolItem copyWith({
    String? name,
    String? argsJson,
    ToolStatus? status,
    String? result,
    ApprovalState? approval,
  }) => ToolItem(
    callId: callId,
    name: name ?? this.name,
    argsJson: argsJson ?? this.argsJson,
    status: status ?? this.status,
    result: result ?? this.result,
    approval: approval ?? this.approval,
  );

  /// Parsed arguments; null while the JSON is incomplete or not an object.
  Map<String, dynamic>? get args {
    try {
      final v = jsonDecode(argsJson);
      return v is Map<String, dynamic> ? v : null;
    } catch (_) {
      return null;
    }
  }

  /// One-line target for the card header: path, command, pattern, url.
  String get target {
    final a = args;
    if (a == null) return '';
    for (final k in const ['path', 'file_path', 'command', 'pattern', 'url']) {
      final v = a[k];
      if (v is String && v.isNotEmpty) {
        return v.split('\n').first;
      }
    }
    return '';
  }
}

enum NoticeKind { error, info }

class NoticeItem extends ChatItem {
  const NoticeItem(this.text, {this.kind = NoticeKind.info});
  final String text;
  final NoticeKind kind;
}

@immutable
class ChatState {
  const ChatState({
    this.items = const [],
    this.running = false,
    this.loading = true,
    this.turnId,
    this.title,
    this.inputTokens = 0,
    this.outputTokens = 0,
    this.reasoningTokens = 0,
    this.cacheReadTokens = 0,
    this.streamingIndex,
    this.toolIndex = const {},
  });

  final List<ChatItem> items;
  final bool running;
  final bool loading;
  final String? turnId;
  final String? title;
  final int inputTokens;
  final int outputTokens;
  final int reasoningTokens;
  final int cacheReadTokens;

  /// Index into [items] of the assistant item receiving deltas.
  final int? streamingIndex;

  /// Model tool-call index -> call id, to route `toolCallArgs` fragments.
  final Map<int, String> toolIndex;

  List<ToolItem> get pendingApprovals => [
    for (final i in items)
      if (i is ToolItem &&
          i.approval != null &&
          !i.approval!.resolved &&
          i.status == ToolStatus.awaitingApproval)
        i,
  ];

  ChatState copyWith({
    List<ChatItem>? items,
    bool? running,
    bool? loading,
    String? turnId,
    String? title,
    int? inputTokens,
    int? outputTokens,
    int? reasoningTokens,
    int? cacheReadTokens,
    Object? streamingIndex = _keep,
    Map<int, String>? toolIndex,
  }) => ChatState(
    items: items ?? this.items,
    running: running ?? this.running,
    loading: loading ?? this.loading,
    turnId: turnId ?? this.turnId,
    title: title ?? this.title,
    inputTokens: inputTokens ?? this.inputTokens,
    outputTokens: outputTokens ?? this.outputTokens,
    reasoningTokens: reasoningTokens ?? this.reasoningTokens,
    cacheReadTokens: cacheReadTokens ?? this.cacheReadTokens,
    streamingIndex: identical(streamingIndex, _keep)
        ? this.streamingIndex
        : streamingIndex as int?,
    toolIndex: toolIndex ?? this.toolIndex,
  );
}

const Object _keep = Object();

/// Build state from `get_session` history. Tool results fold into their calls;
/// pending approvals re-attach so cards survive a reload.
ChatState chatStateFromDetail(SessionDetailDto d) {
  final items = <ChatItem>[];
  final toolAt = <String, int>{};
  for (final m in d.messages) {
    switch (m.role) {
      case RoleDto.system:
        break;
      case RoleDto.user:
        items.add(UserItem(m.content));
      case RoleDto.assistant:
        if (m.content.isNotEmpty || (m.reasoning ?? '').isNotEmpty) {
          items.add(
            AssistantItem(
              text: m.content,
              reasoning: m.reasoning ?? '',
              streaming: false,
            ),
          );
        }
        for (final c in m.toolCalls) {
          toolAt[c.callId] = items.length;
          items.add(
            ToolItem(
              callId: c.callId,
              name: c.name,
              argsJson: c.argumentsJson,
              status: ToolStatus.running,
            ),
          );
        }
      case RoleDto.tool:
        final i = toolAt[m.toolCallId];
        if (i != null) {
          final t = items[i] as ToolItem;
          items[i] = t.copyWith(status: ToolStatus.done, result: m.content);
        }
    }
  }
  for (final p in d.pendingApprovals) {
    final approval = ApprovalState(id: p.approvalId, detail: p.detail);
    final i = toolAt[p.callId];
    if (i != null) {
      items[i] = (items[i] as ToolItem).copyWith(
        status: ToolStatus.awaitingApproval,
        approval: approval,
      );
    } else {
      items.add(
        ToolItem(
          callId: p.callId,
          name: p.tool,
          status: ToolStatus.awaitingApproval,
          approval: approval,
        ),
      );
    }
  }
  return ChatState(
    items: items,
    running: d.session.running,
    loading: false,
    title: d.session.title,
  );
}

/// Fold one live event into the state. Durable events win over deltas.
ChatState reduceEvent(ChatState s, SessionEventDto e) {
  final items = [...s.items];
  int indexOfTool(String? callId) =>
      items.indexWhere((x) => x is ToolItem && x.callId == callId);

  switch (e.kind) {
    case SessionEventKind.turnStarted:
      return s.copyWith(running: true, turnId: e.turnId, streamingIndex: null);

    case SessionEventKind.userMessage:
      items.add(UserItem(e.text ?? ''));
      return s.copyWith(items: items, streamingIndex: null);

    case SessionEventKind.textDelta:
    case SessionEventKind.reasoningDelta:
      var si = s.streamingIndex;
      if (si == null || si >= items.length || items[si] is! AssistantItem) {
        items.add(const AssistantItem());
        si = items.length - 1;
      }
      final a = items[si] as AssistantItem;
      items[si] = e.kind == SessionEventKind.textDelta
          ? a.copyWith(text: a.text + (e.text ?? ''))
          : a.copyWith(reasoning: a.reasoning + (e.text ?? e.reasoning ?? ''));
      return s.copyWith(items: items, streamingIndex: si);

    case SessionEventKind.toolCallStart:
      final id = e.callId ?? '';
      final idx = e.index;
      if (indexOfTool(id) < 0) {
        items.add(ToolItem(callId: id, name: e.name ?? ''));
      }
      return s.copyWith(
        items: items,
        toolIndex: idx == null ? null : {...s.toolIndex, idx: id},
      );

    case SessionEventKind.toolCallArgs:
      final id = s.toolIndex[e.index];
      final i = id == null ? -1 : indexOfTool(id);
      if (i < 0) return s;
      final t = items[i] as ToolItem;
      items[i] = t.copyWith(argsJson: t.argsJson + (e.text ?? ''));
      return s.copyWith(items: items);

    case SessionEventKind.assistantMessage:
      final text = e.text ?? '';
      final reasoning = e.reasoning ?? '';
      final si = s.streamingIndex;
      final has = si != null && si < items.length && items[si] is AssistantItem;
      if (text.isEmpty && reasoning.isEmpty) {
        if (has) items.removeAt(si);
        return s.copyWith(
          items: items,
          streamingIndex: null,
          toolIndex: const {},
        );
      }
      if (has) {
        items[si] = (items[si] as AssistantItem).copyWith(
          text: text,
          reasoning: reasoning,
          streaming: false,
        );
      } else {
        // No deltas seen (e.g. non-streaming provider): insert before this
        // step's tool items so order matches the model's message.
        var at = items.length;
        while (at > 0 &&
            items[at - 1] is ToolItem &&
            (items[at - 1] as ToolItem).status == ToolStatus.streaming) {
          at--;
        }
        items.insert(
          at,
          AssistantItem(text: text, reasoning: reasoning, streaming: false),
        );
      }
      return s.copyWith(items: items, streamingIndex: null, toolIndex: const {});

    case SessionEventKind.toolCall:
      final id = e.callId ?? '';
      final i = indexOfTool(id);
      if (i >= 0) {
        final t = items[i] as ToolItem;
        items[i] = t.copyWith(
          name: e.name,
          argsJson: e.argumentsJson,
          status: t.status == ToolStatus.awaitingApproval
              ? t.status
              : ToolStatus.running,
        );
      } else {
        items.add(
          ToolItem(
            callId: id,
            name: e.name ?? '',
            argsJson: e.argumentsJson ?? '',
            status: ToolStatus.running,
          ),
        );
      }
      return s.copyWith(items: items);

    case SessionEventKind.approvalRequest:
      final id = e.callId ?? '';
      final approval = ApprovalState(id: e.approvalId ?? '', detail: e.detail);
      final i = indexOfTool(id);
      if (i >= 0) {
        items[i] = (items[i] as ToolItem).copyWith(
          status: ToolStatus.awaitingApproval,
          approval: approval,
        );
      } else {
        items.add(
          ToolItem(
            callId: id,
            name: e.tool ?? '',
            status: ToolStatus.awaitingApproval,
            approval: approval,
          ),
        );
      }
      return s.copyWith(items: items);

    case SessionEventKind.approvalResolved:
      final i = items.indexWhere(
        (x) => x is ToolItem && x.approval?.id == e.approvalId,
      );
      if (i < 0) return s;
      final t = items[i] as ToolItem;
      items[i] = t.copyWith(
        status: e.approved ? ToolStatus.running : ToolStatus.denied,
        approval: t.approval!.resolve(approved: e.approved, reason: e.reason),
      );
      return s.copyWith(items: items);

    case SessionEventKind.toolResult:
      final i = indexOfTool(e.callId);
      if (i < 0) return s;
      final t = items[i] as ToolItem;
      items[i] = t.copyWith(
        // A denial already set `denied`; keep it rather than a generic error.
        status: t.status == ToolStatus.denied
            ? ToolStatus.denied
            : (e.isError ? ToolStatus.error : ToolStatus.done),
        result: e.text ?? '',
      );
      return s.copyWith(items: items);

    case SessionEventKind.usage:
      final u = e.usage;
      if (u == null) return s;
      final rt = u.reasoningTokens.toInt();
      // Usage lands after the step's assistant message; tag its reasoning.
      if (rt > 0) {
        for (var i = items.length - 1; i >= 0; i--) {
          final it = items[i];
          if (it is AssistantItem && it.reasoning.isNotEmpty) {
            if (it.reasoningTokens == null) {
              items[i] = it.copyWith(reasoningTokens: rt);
            }
            break;
          }
        }
      }
      return s.copyWith(
        items: items,
        inputTokens: s.inputTokens + u.inputTokens.toInt(),
        outputTokens: s.outputTokens + u.outputTokens.toInt(),
        reasoningTokens: s.reasoningTokens + rt,
        cacheReadTokens: s.cacheReadTokens + u.cacheReadTokens.toInt(),
      );

    case SessionEventKind.turnEnded:
      for (var i = 0; i < items.length; i++) {
        final it = items[i];
        if (it is AssistantItem && it.streaming) {
          items[i] = it.copyWith(streaming: false);
        }
      }
      final note = switch (e.endReason) {
        TurnEndReasonDto.cancelled => 'Turn cancelled',
        TurnEndReasonDto.maxSteps => 'Stopped: step limit reached',
        _ => null,
      };
      if (note != null) items.add(NoticeItem(note));
      return s.copyWith(
        items: items,
        running: false,
        streamingIndex: null,
        toolIndex: const {},
      );

    case SessionEventKind.titleChanged:
      return s.copyWith(title: e.text);

    case SessionEventKind.error:
      items.add(NoticeItem(e.text ?? 'Unknown error', kind: NoticeKind.error));
      return s.copyWith(items: items, running: e.fatal ? false : s.running);
  }
}

/// Line diff rows for edit/write tool cards.
enum DiffKind { add, remove, context }

@immutable
class DiffLine {
  const DiffLine(this.kind, this.text);
  final DiffKind kind;
  final String text;
}

/// Diff for `edit` (old_string -> new_string) or `write` (all lines added).
/// Null for any other tool or while the arguments are still incomplete.
List<DiffLine>? diffForTool(String name, Map<String, dynamic>? args) {
  if (args == null) return null;
  List<DiffLine> lines(String s, DiffKind k) => [
    for (final l in s.split('\n')) DiffLine(k, l),
  ];
  if (name == 'edit') {
    final o = args['old_string'];
    final n = args['new_string'];
    if (o is! String || n is! String) return null;
    return _trimCommon(lines(o, DiffKind.remove), lines(n, DiffKind.add));
  }
  if (name == 'write') {
    final c = args['content'];
    if (c is! String) return null;
    return lines(c, DiffKind.add);
  }
  return null;
}

/// Shared leading/trailing lines become context so the changed part stands out.
List<DiffLine> _trimCommon(List<DiffLine> old, List<DiffLine> neu) {
  var head = 0;
  while (head < old.length &&
      head < neu.length &&
      old[head].text == neu[head].text) {
    head++;
  }
  var tail = 0;
  while (tail < old.length - head &&
      tail < neu.length - head &&
      old[old.length - 1 - tail].text == neu[neu.length - 1 - tail].text) {
    tail++;
  }
  return [
    for (var i = 0; i < head; i++) DiffLine(DiffKind.context, old[i].text),
    ...old.sublist(head, old.length - tail),
    ...neu.sublist(head, neu.length - tail),
    for (var i = old.length - tail; i < old.length; i++)
      DiffLine(DiffKind.context, old[i].text),
  ];
}
