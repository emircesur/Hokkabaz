import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';
import 'composer_logic.dart';
import 'model_picker.dart';
import 'workspace_files.dart';

/// Composer: text field, mode + model chips, `/` commands, `@` file mentions.
class Composer extends ConsumerStatefulWidget {
  const Composer({super.key, required this.sessionId, required this.running});

  final String sessionId;
  final bool running;

  @override
  ConsumerState<Composer> createState() => _ComposerState();
}

class _Entry {
  const _Entry(this.label, this.detail, this.icon, this.onSelect);
  final String label;
  final String? detail;
  final IconData icon;
  final VoidCallback onSelect;
}

class _ComposerState extends ConsumerState<Composer> {
  final _text = TextEditingController();
  final _focus = FocusNode();
  int _sel = 0;
  String _lastKey = '';
  bool _dismissed = false;

  @override
  void initState() {
    super.initState();
    _text.addListener(_onText);
  }

  @override
  void dispose() {
    _text.removeListener(_onText);
    _text.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _onText() {
    // Rebuild so menus follow text and cursor; selection resets per query.
    final key = '${_text.text}|${_text.selection.baseOffset}';
    if (key != _lastKey) {
      _lastKey = key;
      _sel = 0;
      _dismissed = false;
      setState(() {});
    }
  }

  void _send() {
    final t = _text.text;
    if (t.trim().isEmpty) return;
    _text.clear();
    ref
        .read(chatControllerProvider(widget.sessionId).notifier)
        .send(
          t,
          ref.read(chatModeProvider),
          model: ref.read(composerModelProvider),
        );
    _focus.requestFocus();
  }

  void _runSlash(String name) {
    _text.clear();
    switch (name) {
      case 'plan':
        ref.read(chatModeProvider.notifier).set(ModeDto.plan);
      case 'build':
        ref.read(chatModeProvider.notifier).set(ModeDto.build);
      case 'model':
        showModelPicker(context);
      case 'clear':
        ref.read(sessionListProvider.notifier).create();
    }
    _focus.requestFocus();
  }

  void _insertMention(MentionToken tok, String path) {
    final cursor = _text.selection.baseOffset;
    final r = applyMention(_text.text, tok, cursor, path);
    _text.value = TextEditingValue(
      text: r.text,
      selection: TextSelection.collapsed(offset: r.cursor),
    );
    _focus.requestFocus();
  }

  /// Selectable menu rows for the current text, plus whether a `@` menu is open.
  ({List<_Entry> entries, bool mention}) _menu() {
    if (_dismissed) return (entries: const [], mention: false);
    final text = _text.text;
    final slash = slashQuery(text);
    if (slash != null) {
      return (
        entries: [
          for (final c in filterSlash(slash))
            _Entry(
              '/${c.name}',
              c.help,
              Icons.terminal,
              () => _runSlash(c.name),
            ),
        ],
        mention: false,
      );
    }
    final cursor = _text.selection.baseOffset;
    final tok = fileMentionsSupported ? detectMention(text, cursor) : null;
    if (tok == null) return (entries: const [], mention: false);
    final files = ref.watch(workspaceFilesProvider).value ?? const <String>[];
    return (
      entries: [
        for (final f in fuzzyFilter(tok.query, files))
          _Entry(
            f,
            null,
            Icons.insert_drive_file_outlined,
            () => _insertMention(tok, f),
          ),
      ],
      mention: true,
    );
  }

  String _modelLabel() {
    final picked = ref.watch(composerModelProvider);
    final models = ref.watch(modelListProvider).value?.models ?? const [];
    final String? reference =
        picked ??
        ref
            .watch(sessionListProvider)
            .value
            ?.where((s) => s.id == widget.sessionId)
            .firstOrNull
            ?.model;
    if (reference == null) return 'Default model';
    final m = models.where((m) => m.reference == reference).firstOrNull;
    return m?.name ?? reference.split('/').last;
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final mode = ref.watch(chatModeProvider);
    final menu = _menu();
    final entries = menu.entries;
    final sel = entries.isEmpty ? 0 : _sel.clamp(0, entries.length - 1);
    final menuOpen = entries.isNotEmpty;

    void move(int d) {
      if (entries.isEmpty) return;
      setState(() => _sel = (sel + d) % entries.length);
    }

    final ctrlEnter = [
      const SingleActivator(LogicalKeyboardKey.enter, control: true),
      const SingleActivator(LogicalKeyboardKey.enter, meta: true),
    ];
    final bindings = <ShortcutActivator, VoidCallback>{
      for (final a in ctrlEnter) a: _send,
      if (menuOpen) ...{
        const SingleActivator(LogicalKeyboardKey.arrowDown): () => move(1),
        const SingleActivator(LogicalKeyboardKey.arrowUp): () => move(-1),
        const SingleActivator(LogicalKeyboardKey.tab): () =>
            entries[sel].onSelect(),
        const SingleActivator(LogicalKeyboardKey.enter): () =>
            entries[sel].onSelect(),
        // Closes the menu without touching the text; a second Esc cancels.
        const SingleActivator(LogicalKeyboardKey.escape): () =>
            setState(() => _dismissed = true),
      } else
        // Enter sends; Shift+Enter keeps the newline.
        const SingleActivator(LogicalKeyboardKey.enter): _send,
    };

    final narrow = isNarrow(context);
    final density = narrow ? VisualDensity.standard : VisualDensity.compact;
    // Own the bottom inset: home-indicator safe area when the composer sits at
    // the screen edge; the shell Scaffold already lifts it above the keyboard.
    return Container(
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(top: BorderSide(color: hk.border)),
      ),
      child: SafeArea(
        top: false,
        child: Padding(
          padding: const EdgeInsets.all(HkTokens.s3),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              if (menu.mention || menuOpen)
                _MenuList(
                  entries: entries,
                  selected: sel,
                  footer: menu.mention
                      ? 'MCP resources: not available yet'
                      : null,
                ),
              Wrap(
                spacing: HkTokens.s2,
                runSpacing: HkTokens.s1,
                crossAxisAlignment: WrapCrossAlignment.center,
                children: [
                  ActionChip(
                    visualDensity: density,
                    avatar: Icon(
                      mode == ModeDto.plan
                          ? Icons.map_outlined
                          : Icons.build_outlined,
                      size: 16,
                    ),
                    label: Text(mode == ModeDto.plan ? 'Plan' : 'Build'),
                    tooltip: 'Toggle plan / build',
                    onPressed: () => ref
                        .read(chatModeProvider.notifier)
                        .set(
                          mode == ModeDto.plan ? ModeDto.build : ModeDto.plan,
                        ),
                  ),
                  ActionChip(
                    visualDensity: density,
                    avatar: const Icon(Icons.memory_outlined, size: 16),
                    label: Text(_modelLabel()),
                    tooltip: 'Choose model',
                    onPressed: () => showModelPicker(context),
                  ),
                  if (widget.running)
                    Text(
                      'Running: your message steers the current turn',
                      style: TextStyle(fontSize: 12, color: hk.textMuted),
                    ),
                ],
              ),
              const SizedBox(height: HkTokens.s2),
              Row(
                crossAxisAlignment: CrossAxisAlignment.end,
                children: [
                  Expanded(
                    child: CallbackShortcuts(
                      bindings: bindings,
                      child: TextField(
                        controller: _text,
                        focusNode: _focus,
                        minLines: 1,
                        maxLines: 6,
                        textInputAction: TextInputAction.newline,
                        decoration: InputDecoration(
                          isDense: !narrow,
                          hintText: fileMentionsSupported
                              ? 'Message  ( / commands, @ files )'
                              : 'Message  ( / commands )',
                          border: const OutlineInputBorder(),
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(width: HkTokens.s2),
                  IconButton.filled(
                    tooltip: 'Send (Ctrl+Enter)',
                    icon: const Icon(Icons.arrow_upward),
                    onPressed: _send,
                  ),
                ],
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _MenuList extends StatelessWidget {
  const _MenuList({required this.entries, required this.selected, this.footer});

  final List<_Entry> entries;
  final int selected;
  final String? footer;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      margin: const EdgeInsets.only(bottom: HkTokens.s2),
      constraints: const BoxConstraints(maxHeight: 240),
      decoration: BoxDecoration(
        color: hk.bg,
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: ListView(
        shrinkWrap: true,
        padding: EdgeInsets.zero,
        children: [
          for (var i = 0; i < entries.length; i++)
            ListTile(
              dense: true,
              minTileHeight: touchMinHeight(context) > 0 ? kTouchTarget : 36,
              selected: i == selected,
              leading: Icon(entries[i].icon, size: 16),
              title: Text(entries[i].label, overflow: TextOverflow.ellipsis),
              subtitle: entries[i].detail == null
                  ? null
                  : Text(
                      entries[i].detail!,
                      style: TextStyle(fontSize: 11, color: hk.textMuted),
                    ),
              onTap: entries[i].onSelect,
            ),
          if (footer != null)
            ListTile(
              dense: true,
              minTileHeight: 32,
              enabled: false,
              leading: const Icon(Icons.hub_outlined, size: 16),
              title: Text(footer!, style: const TextStyle(fontSize: 12)),
            ),
        ],
      ),
    );
  }
}
