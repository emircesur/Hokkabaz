import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_riverpod/misc.dart' show Override;
import 'package:flutter_test/flutter_test.dart';
import 'package:hokkabaz/app/breakpoints.dart';
import 'package:hokkabaz/app/shell.dart';
import 'package:hokkabaz/ffi/notifications.dart';
import 'package:hokkabaz/screens/chat/chat_model.dart';
import 'package:hokkabaz/screens/chat/chat_providers.dart';
import 'package:hokkabaz/screens/chat_screen.dart';
import 'package:hokkabaz/screens/flows/flow_canvas.dart';
import 'package:hokkabaz/screens/flows/model/flow_editor.dart';
import 'package:hokkabaz/screens/models/models_screen.dart';
import 'package:hokkabaz/ffi/settings_providers.dart';
import 'package:hokkabaz/src/rust/dto.dart';
import 'package:hokkabaz/theme/theme.dart';

class _FakeSessions extends SessionList {
  @override
  Future<List<SessionInfoDto>> build() async => [
    SessionInfoDto(
      id: 's1',
      title: 'fix webhook',
      model: 'deepseek/deepseek-chat',
      createdMs: 0,
      updatedMs: DateTime.now().millisecondsSinceEpoch,
      eventCount: 3,
      turnCount: 1,
      running: false,
    ),
  ];
}

class _FakeChat extends ChatController {
  _FakeChat(super.sessionId);

  @override
  ChatState build() => const ChatState(
    loading: false,
    running: false,
    title: 'fix webhook',
    inputTokens: 1200,
    outputTokens: 340,
    items: [
      UserItem('please run the tests'),
      AssistantItem(text: 'Running them now.', streaming: false),
      ToolItem(
        callId: 'c1',
        name: 'bash',
        argsJson: '{"command":"cargo test -p hk-flow --lib some_long_filter"}',
        status: ToolStatus.awaitingApproval,
        approval: ApprovalState(id: 'a1', detail: 'cargo test -p hk-flow'),
      ),
    ],
  );
}

class _Select extends SelectedSession {
  _Select(this.id);
  final String? id;

  @override
  String? build() => id;
}

class _RecordingNotifier implements ApprovalNotifier {
  final shown = <String>[];
  final cancelled = <String>[];

  @override
  Future<void> show(ApprovalNotice notice) async =>
      shown.add(notice.approvalId);

  @override
  Future<void> cancel(String approvalId) async => cancelled.add(approvalId);
}

void _phone(WidgetTester t, {double w = 390, double h = 844}) {
  t.view.physicalSize = Size(w, h);
  t.view.devicePixelRatio = 1;
  addTearDown(t.view.reset);
}

Widget _app(Widget home, {List<Override> overrides = const []}) =>
    ProviderScope(
      overrides: [...overrides],
      child: MaterialApp(theme: buildTheme(Brightness.dark), home: home),
    );

void main() {
  group('breakpoints', () {
    test('rail threshold', () {
      expect(isNarrowWidth(390), isTrue);
      expect(isNarrowWidth(719.9), isTrue);
      expect(isNarrowWidth(kRailBreakpoint), isFalse);
      expect(kChatTwoPaneMin, kRailBreakpoint);
      expect(kChatInspectorMin, greaterThan(kFlowsWideMin));
    });
  });

  group('shell', () {
    testWidgets('phone: bottom bar with More, no rail', (t) async {
      _phone(t);
      await t.pumpWidget(
        _app(const AppShell(location: '/models', child: SizedBox.expand())),
      );
      expect(find.byType(NavigationBar), findsOneWidget);
      expect(find.byType(NavigationRail), findsNothing);
      expect(find.text('More'), findsOneWidget);
      await t.tap(find.text('More'));
      await t.pumpAndSettle();
      expect(find.text('Settings'), findsOneWidget);
      expect(t.takeException(), isNull);
    });

    testWidgets('desktop: rail', (t) async {
      _phone(t, w: 1100, h: 800);
      await t.pumpWidget(
        _app(const AppShell(location: '/chat', child: SizedBox.expand())),
      );
      expect(find.byType(NavigationRail), findsOneWidget);
      expect(find.byType(NavigationBar), findsNothing);
    });
  });

  group('chat on phone', () {
    final overrides = [
      sessionListProvider.overrideWith(_FakeSessions.new),
      chatControllerProvider.overrideWith2(_FakeChat.new),
      modelListProvider.overrideWith(
        (ref) async => const ModelListDto(models: [], errors: []),
      ),
    ];

    testWidgets('session list first, bottom bar visible', (t) async {
      _phone(t);
      await t.pumpWidget(
        _app(
          const AppShell(
            location: '/chat',
            child: ChatScreen(title: 'Chat', icon: Icons.chat),
          ),
          overrides: overrides,
        ),
      );
      await t.pumpAndSettle();
      expect(find.text('fix webhook'), findsOneWidget);
      expect(find.byType(NavigationBar), findsOneWidget);
      expect(find.byType(TextField), findsNothing);
      expect(t.takeException(), isNull);
    });

    testWidgets('conversation is full screen; targets >= 48dp', (t) async {
      _phone(t);
      await t.pumpWidget(
        _app(
          const AppShell(
            location: '/chat',
            child: ChatScreen(title: 'Chat', icon: Icons.chat),
          ),
          overrides: [
            ...overrides,
            selectedSessionProvider.overrideWith(() => _Select('s1')),
          ],
        ),
      );
      await t.pumpAndSettle();
      expect(find.byType(NavigationBar), findsNothing);
      expect(find.byIcon(Icons.arrow_back), findsOneWidget);
      for (final label in ['Approve', 'Deny']) {
        final size = t.getSize(
          find.ancestor(
            of: find.text(label),
            matching: find.byWidgetPredicate((w) => w is ButtonStyleButton),
          ),
        );
        expect(size.height, greaterThanOrEqualTo(kTouchTarget), reason: label);
      }
      expect(t.takeException(), isNull);
    });

    testWidgets('composer stays above the keyboard', (t) async {
      _phone(t);
      await t.pumpWidget(
        _app(
          const AppShell(
            location: '/chat',
            child: ChatScreen(title: 'Chat', icon: Icons.chat),
          ),
          overrides: [
            ...overrides,
            selectedSessionProvider.overrideWith(() => _Select('s1')),
          ],
        ),
      );
      await t.pumpAndSettle();
      t.view.viewInsets = const FakeViewPadding(bottom: 320);
      addTearDown(t.view.resetViewInsets);
      await t.pumpAndSettle();
      final bottom = t.getBottomLeft(find.byType(TextField).last).dy;
      expect(bottom, lessThanOrEqualTo(844 - 320));
      expect(t.takeException(), isNull);
    });

    testWidgets('inspector opens as bottom sheet', (t) async {
      _phone(t);
      await t.pumpWidget(
        _app(
          const AppShell(
            location: '/chat',
            child: ChatScreen(title: 'Chat', icon: Icons.chat),
          ),
          overrides: [
            ...overrides,
            selectedSessionProvider.overrideWith(() => _Select('s1')),
          ],
        ),
      );
      await t.pumpAndSettle();
      await t.tap(find.byTooltip('Inspector'));
      await t.pumpAndSettle();
      expect(find.byType(DraggableScrollableSheet), findsOneWidget);
    });
  });

  group('approvals', () {
    testWidgets('inbox drives sink, badge and banner', (t) async {
      _phone(t);
      final sink = _RecordingNotifier();
      late WidgetRef captured;
      await t.pumpWidget(
        _app(
          Consumer(
            builder: (_, ref, _) {
              captured = ref;
              return const AppShell(
                location: '/models',
                child: SizedBox.expand(),
              );
            },
          ),
          overrides: [approvalNotifierProvider.overrideWithValue(sink)],
        ),
      );
      const n = ApprovalNotice(sessionId: 's1', approvalId: 'a1', tool: 'bash');
      captured.read(approvalInboxProvider.notifier).sync('s1', [n]);
      await t.pump();
      expect(sink.shown, ['a1']);
      expect(find.textContaining('Approval needed: bash'), findsOneWidget);
      expect(find.byType(Badge), findsOneWidget);
      captured.read(approvalInboxProvider.notifier).sync('s1', []);
      await t.pump();
      expect(sink.cancelled, ['a1']);
      expect(find.textContaining('Approval needed'), findsNothing);
    });
  });

  group('flow canvas on phone', () {
    testWidgets('long-press node multi-selects; ports have touch hit boxes', (
      t,
    ) async {
      _phone(t);
      final ed = FlowEditor();
      ed.addRawNode(
        type: 'n8n-nodes-base.noOp',
        version: 1,
        baseName: 'A',
        at: (x: 20, y: 20),
      );
      ed.addRawNode(
        type: 'n8n-nodes-base.noOp',
        version: 1,
        baseName: 'B',
        at: (x: 20, y: 200),
      );
      ed.clearSelection();
      await t.pumpWidget(
        _app(
          Scaffold(
            body: FlowCanvas(
              editor: ed,
              types: const {},
              onAddNode: (_, _) {},
              onOpenNode: (_) {},
              onMessage: (_) {},
              onSave: () {},
            ),
          ),
        ),
      );
      await t.pump();
      await t.longPress(find.text('A'));
      await t.tap(find.text('B'));
      await t.pump(const Duration(milliseconds: 400));
      expect(ed.selection.length, 2);
      expect(t.takeException(), isNull);
    });
  });

  group('models on phone', () {
    testWidgets('no overflow', (t) async {
      _phone(t);
      final settings = SettingsDto(
        providers: const [
          ProviderDto(
            id: 'deepseek',
            kind: ProviderKindDto.deepseek,
            enabled: true,
            hasKey: false,
            builtin: true,
            models: [],
          ),
        ],
        mode: ModeDto.build,
        workspaceRoot: '/data/user/0/app/workspace',
        profile: CoreProfile.mobile,
        dataDir: '/data/user/0/app',
      );
      await t.pumpWidget(
        _app(
          const Scaffold(body: ModelsScreen()),
          overrides: [
            settingsProvider.overrideWith(() => _FakeSettings(settings)),
            modelListProvider.overrideWith(
              (ref) async => const ModelListDto(models: [], errors: []),
            ),
          ],
        ),
      );
      await t.pumpAndSettle();
      expect(find.text('Models'), findsWidgets);
      expect(t.takeException(), isNull);
    });
  });
}

class _FakeSettings extends SettingsController {
  _FakeSettings(this.s);
  final SettingsDto s;

  @override
  Future<SettingsDto> build() async => s;
}
