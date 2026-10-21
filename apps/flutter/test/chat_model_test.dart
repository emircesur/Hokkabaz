import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/chat/chat_model.dart';
import 'package:hokkabaz/src/rust/dto.dart';

SessionEventDto ev(
  SessionEventKind kind, {
  String? turnId,
  String? text,
  int? index,
  String? callId,
  String? name,
  String? argumentsJson,
  String? approvalId,
  String? tool,
  String? detail,
  bool isError = false,
  TurnEndReasonDto? endReason,
}) => SessionEventDto(
  kind: kind,
  turnId: turnId,
  text: text,
  index: index,
  callId: callId,
  name: name,
  argumentsJson: argumentsJson,
  approvalId: approvalId,
  tool: tool,
  detail: detail,
  isError: isError,
  approved: false,
  fatal: false,
  endReason: endReason,
);

ChatState fold(Iterable<SessionEventDto> events, [ChatState? from]) =>
    events.fold(from ?? const ChatState(loading: false), reduceEvent);

void main() {
  test('text deltas accumulate into one streaming assistant item', () {
    final s = fold([
      ev(SessionEventKind.turnStarted, turnId: 't1'),
      ev(SessionEventKind.textDelta, text: 'Hel'),
      ev(SessionEventKind.textDelta, text: 'lo'),
    ]);
    expect(s.running, isTrue);
    expect(s.items, hasLength(1));
    final a = s.items.single as AssistantItem;
    expect(a.text, 'Hello');
    expect(a.streaming, isTrue);

    final done = reduceEvent(
      s,
      ev(SessionEventKind.assistantMessage, text: 'Hello!'),
    );
    final d = done.items.single as AssistantItem;
    expect(d.text, 'Hello!');
    expect(d.streaming, isFalse);
  });

  test('tool call start, args, result', () {
    final s = fold([
      ev(SessionEventKind.toolCallStart, index: 0, callId: 'c1', name: 'read'),
      ev(SessionEventKind.toolCallArgs, index: 0, text: '{"path":'),
      ev(SessionEventKind.toolCallArgs, index: 0, text: '"a.txt"}'),
      ev(
        SessionEventKind.toolCall,
        callId: 'c1',
        name: 'read',
        argumentsJson: '{"path":"a.txt"}',
      ),
    ]);
    var t = s.items.single as ToolItem;
    expect(t.status, ToolStatus.running);
    expect(t.target, 'a.txt');

    final r = reduceEvent(
      s,
      ev(SessionEventKind.toolResult, callId: 'c1', text: 'ok'),
    );
    t = r.items.single as ToolItem;
    expect(t.status, ToolStatus.done);
    expect(t.result, 'ok');

    final e = reduceEvent(
      s,
      ev(
        SessionEventKind.toolResult,
        callId: 'c1',
        text: 'boom',
        isError: true,
      ),
    );
    expect((e.items.single as ToolItem).status, ToolStatus.error);
  });

  test('approval request attaches to its call and blocks on pending', () {
    final s = fold([
      ev(SessionEventKind.toolCallStart, index: 0, callId: 'c1', name: 'bash'),
      ev(
        SessionEventKind.approvalRequest,
        callId: 'c1',
        approvalId: 'a1',
        tool: 'bash',
        detail: 'rm -rf x',
      ),
    ]);
    final t = s.items.single as ToolItem;
    expect(t.status, ToolStatus.awaitingApproval);
    expect(t.approval!.id, 'a1');
    expect(t.approval!.detail, 'rm -rf x');
    expect(s.pendingApprovals, hasLength(1));
  });

  test('turn end stops running, freezes streaming, notes cancellation', () {
    final s = fold([
      ev(SessionEventKind.turnStarted, turnId: 't1'),
      ev(SessionEventKind.textDelta, text: 'partial'),
      ev(
        SessionEventKind.turnEnded,
        turnId: 't1',
        endReason: TurnEndReasonDto.cancelled,
      ),
    ]);
    expect(s.running, isFalse);
    expect((s.items.first as AssistantItem).streaming, isFalse);
    expect(s.items.last, isA<NoticeItem>());

    final ok = fold([
      ev(SessionEventKind.turnStarted),
      ev(SessionEventKind.turnEnded, endReason: TurnEndReasonDto.completed),
    ]);
    expect(ok.running, isFalse);
    expect(ok.items, isEmpty);
  });
}
