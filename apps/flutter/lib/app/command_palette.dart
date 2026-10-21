import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../ffi/settings_providers.dart';
import '../screens/chat/chat_providers.dart';
import '../screens/chat/composer_logic.dart';
import '../src/rust/dto.dart';
import '../theme/tokens.dart';
import 'destinations.dart';
import 'theme_mode.dart';

class PaletteCommand {
  const PaletteCommand(this.group, this.title, this.icon, this.run);
  final String group;
  final String title;
  final IconData icon;
  final VoidCallback run;
}

/// Creates a session and shows it in the Chat tab.
Future<void> startNewSession(BuildContext context, WidgetRef ref) async {
  context.go('/chat');
  await ref.read(sessionListProvider.notifier).create();
}

/// Cancels the running turn of the open session, if any.
void cancelCurrentTurn(WidgetRef ref) {
  final id = ref.read(selectedSessionProvider);
  if (id == null) return;
  if (ref.read(chatControllerProvider(id)).running) {
    ref.read(chatControllerProvider(id).notifier).cancel();
  }
}

List<PaletteCommand> buildPaletteCommands(BuildContext context, WidgetRef ref) {
  final models = ref.read(modelListProvider).value?.models ?? const [];
  return [
    for (final d in destinations)
      PaletteCommand('Go to', d.label, d.icon, () => context.go(d.path)),
    PaletteCommand(
      'Session',
      'New session',
      Icons.add,
      () => startNewSession(context, ref),
    ),
    PaletteCommand(
      'Mode',
      'Switch to Plan mode',
      Icons.map_outlined,
      () => ref.read(chatModeProvider.notifier).set(ModeDto.plan),
    ),
    PaletteCommand(
      'Mode',
      'Switch to Build mode',
      Icons.build_outlined,
      () => ref.read(chatModeProvider.notifier).set(ModeDto.build),
    ),
    PaletteCommand(
      'Model',
      'Session default model',
      Icons.auto_awesome_outlined,
      () => ref.read(composerModelProvider.notifier).set(null),
    ),
    for (final m in models)
      PaletteCommand(
        'Model',
        '${m.name}  (${m.reference})',
        Icons.memory_outlined,
        () => ref.read(composerModelProvider.notifier).set(m.reference),
      ),
    for (final t in ThemeMode.values)
      PaletteCommand(
        'Theme',
        '${themeModeLabel(t)} theme',
        themeModeIcon(t),
        () => ref.read(themeModeProvider.notifier).set(t),
      ),
  ];
}

Future<void> openCommandPalette(BuildContext context, WidgetRef ref) {
  final commands = buildPaletteCommands(context, ref);
  return showDialog<void>(
    context: context,
    builder: (_) => _Palette(commands: commands),
  );
}

class _Palette extends StatefulWidget {
  const _Palette({required this.commands});
  final List<PaletteCommand> commands;

  @override
  State<_Palette> createState() => _PaletteState();
}

class _PaletteState extends State<_Palette> {
  String _q = '';
  int _sel = 0;

  List<PaletteCommand> get _shown {
    if (_q.isEmpty) return widget.commands;
    final scored = <(PaletteCommand, double)>[];
    for (final c in widget.commands) {
      final s = fuzzyScore(_q, '${c.group} ${c.title}');
      if (s != null) scored.add((c, s));
    }
    scored.sort((a, b) => b.$2.compareTo(a.$2));
    return [for (final s in scored) s.$1];
  }

  void _run(PaletteCommand c) {
    Navigator.of(context).pop();
    c.run();
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final shown = _shown;
    final sel = shown.isEmpty ? 0 : _sel.clamp(0, shown.length - 1);
    void move(int d) {
      if (shown.isEmpty) return;
      setState(() => _sel = (sel + d) % shown.length);
    }

    return Dialog(
      alignment: Alignment.topCenter,
      insetPadding: const EdgeInsets.fromLTRB(
        HkTokens.s4,
        HkTokens.s6 * 2,
        HkTokens.s4,
        HkTokens.s4,
      ),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520, maxHeight: 440),
        child: Padding(
          padding: const EdgeInsets.all(HkTokens.s3),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              CallbackShortcuts(
                bindings: {
                  const SingleActivator(LogicalKeyboardKey.arrowDown): () =>
                      move(1),
                  const SingleActivator(LogicalKeyboardKey.arrowUp): () =>
                      move(-1),
                },
                child: TextField(
                  autofocus: true,
                  decoration: const InputDecoration(
                    isDense: true,
                    prefixIcon: Icon(Icons.search),
                    hintText: 'Type a command',
                    border: OutlineInputBorder(),
                  ),
                  onChanged: (v) => setState(() {
                    _q = v;
                    _sel = 0;
                  }),
                  onSubmitted: (_) {
                    if (shown.isNotEmpty) _run(shown[sel]);
                  },
                ),
              ),
              const SizedBox(height: HkTokens.s2),
              Flexible(
                child: shown.isEmpty
                    ? Padding(
                        padding: const EdgeInsets.all(HkTokens.s3),
                        child: Text(
                          'No matches',
                          style: TextStyle(color: hk.textMuted),
                        ),
                      )
                    : ListView.builder(
                        shrinkWrap: true,
                        itemCount: shown.length,
                        itemBuilder: (_, i) => ListTile(
                          dense: true,
                          minTileHeight: 40,
                          selected: i == sel,
                          leading: Icon(shown[i].icon, size: 18),
                          title: Text(
                            shown[i].title,
                            overflow: TextOverflow.ellipsis,
                          ),
                          trailing: Text(
                            shown[i].group,
                            style: TextStyle(fontSize: 11, color: hk.textMuted),
                          ),
                          onTap: () => _run(shown[i]),
                        ),
                      ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// App-wide keys: Ctrl/Cmd+K palette, Ctrl/Cmd+N new session, Esc cancel turn.
/// The composer binds Ctrl+Enter itself; its menus take Esc first.
class AppShortcuts extends ConsumerWidget {
  const AppShortcuts({super.key, required this.child});

  final Widget child;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    return CallbackShortcuts(
      bindings: {
        for (final ctrl in const [true, false]) ...{
          SingleActivator(
            LogicalKeyboardKey.keyK,
            control: ctrl,
            meta: !ctrl,
          ): () =>
              openCommandPalette(context, ref),
          SingleActivator(
            LogicalKeyboardKey.keyN,
            control: ctrl,
            meta: !ctrl,
          ): () =>
              startNewSession(context, ref),
        },
        const SingleActivator(LogicalKeyboardKey.escape): () =>
            cancelCurrentTurn(ref),
      },
      // Autofocus gives shortcuts a target before the user clicks anything.
      child: Focus(autofocus: true, child: child),
    );
  }
}
