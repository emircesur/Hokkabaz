import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/screens/chat/chat_model.dart';
import 'package:hokkabaz/screens/chat/inspector_model.dart';
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
  UsageDto? usage,
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
  usage: usage,
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

  SessionEventDto call(String id, String name, String args) => ev(
    SessionEventKind.toolCall,
    callId: id,
    name: name,
    argumentsJson: args,
  );
  SessionEventDto result(String id, String text, {bool err = false}) =>
      ev(SessionEventKind.toolResult, callId: id, text: text, isError: err);

  test('usage accumulates totals, steps and latest context size', () {
    UsageDto u(int i, int o) => UsageDto(
      inputTokens: i,
      outputTokens: o,
      cacheReadTokens: 0,
      cacheWriteTokens: 0,
      reasoningTokens: 0,
    );
    final s = fold([
      ev(SessionEventKind.usage, usage: u(100, 10)),
      ev(SessionEventKind.usage, usage: u(150, 20)),
    ]);
    expect(s.inputTokens, 250);
    expect(s.outputTokens, 30);
    expect(s.usageSteps, 2);
    expect(s.lastInputTokens, 150);
  });

  test('todos: latest todo call wins, partial args ignored', () {
    final s = fold([
      call('t1', 'todo', '{"todos":[{"content":"a","status":"pending"}]}'),
      call(
        't2',
        'todo',
        '{"todos":[{"content":"a","status":"completed"},'
            '{"content":"b","status":"in_progress"}]}',
      ),
      ev(SessionEventKind.toolCallStart, index: 0, callId: 't3', name: 'todo'),
      ev(SessionEventKind.toolCallArgs, index: 0, text: '{"todos":[{"con'),
    ]);
    final todos = todosFromItems(s.items);
    expect(todos.map((t) => t.content), ['a', 'b']);
    expect(todos.first.status, TodoStatus.completed);
    expect(todos.last.status, TodoStatus.inProgress);
    expect(todosFromItems(const []), isEmpty);
  });

  test('subagents: task calls with child session parsed from result', () {
    final s = fold([
      call('k1', 'task', '{"description":"Scan repo","prompt":"x"}'),
      call('k2', 'task', '{"prompt":"line one\\nline two","mode":"plan"}'),
      result('k1', 'answer\n\n[subagent session ses_42]'),
    ]);
    final runs = subagentsFromItems(s.items);
    expect(runs, hasLength(2));
    expect(runs[0].title, 'Scan repo');
    expect(runs[0].sessionId, 'ses_42');
    expect(runs[0].status, ToolStatus.done);
    expect(runs[1].title, 'line one');
    expect(runs[1].mode, 'plan');
    expect(runs[1].sessionId, isNull);
  });

  test('changed files: merge edits per path, skip failed and non-edit', () {
    final s = fold([
      call('e1', 'edit', '{"path":"a.rs","old_string":"x","new_string":"y"}'),
      call('w1', 'write', '{"path":"b.rs","content":"l1\\nl2"}'),
      call('e2', 'edit', '{"path":"a.rs","old_string":"p","new_string":"q"}'),
      call('e3', 'edit', '{"path":"c.rs","old_string":"p","new_string":"q"}'),
      call('r1', 'read', '{"path":"d.rs"}'),
      result('e3', 'nope', err: true),
    ]);
    final files = changedFilesFromItems(s.items);
    expect(files.map((f) => f.path), ['a.rs', 'b.rs']);
    expect(files[0].edits, 2);
    expect(files[0].added, 2);
    expect(files[0].removed, 2);
    expect(files[1].added, 2);
    expect(files[1].removed, 0);
  });
}
