import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../app/breakpoints.dart';
import '../../ffi/settings_providers.dart';
import '../../src/rust/dto.dart';
import '../../ffi/voice.dart';
import '../../theme/tokens.dart';
import 'chat_providers.dart';
import 'composer_logic.dart';
import 'model_picker.dart';
import 'workspace_files.dart';

/// Composer: text field, mode + model chips, `/` commands, `@` file mentions.
class Composer extends ConsumerStatefulWidget {
  const Composer({
    super.key,
    required this.sessionId,
    required this.running,
    this.promptPrefix,
  });

  final String sessionId;
  final bool running;

  /// Text added before every prompt (e.g. the Studio project the agent works on).
  final String Function()? promptPrefix;

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
    _focus.addListener(() => setState(() {}));
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

  /// Image files attached to the next message.
  final List<String> _images = [];

  /// Push-to-talk state: idle, recording, transcribing.
  String _voice = 'idle';

  Future<void> _toggleVoice() async {
    try {
      if (_voice == 'idle') {
        await Voice.instance.start();
        setState(() => _voice = 'recording');
      } else if (_voice == 'recording') {
        setState(() => _voice = 'transcribing');
        final t = await Voice.instance.stopAndTranscribe();
        if (t.isNotEmpty) {
          final cur = _text.text;
          _text.text = cur.isEmpty ? t : '$cur $t';
          _text.selection = TextSelection.collapsed(offset: _text.text.length);
        }
        if (mounted) setState(() => _voice = 'idle');
      }
    } catch (e) {
      if (mounted) {
        setState(() => _voice = 'idle');
        ScaffoldMessenger.of(context)
            .showSnackBar(SnackBar(content: Text('Voice: $e')));
      }
    }
  }

  Future<void> _attach() async {
    final r = await FilePicker.pickFiles(
      type: FileType.image,
      dialogTitle: 'Attach an image',
    );
    final p = r.firstOrNull?.path;
    if (p != null) setState(() => _images.add(p));
  }

  void _send() {
    final t = _text.text;
    if (_images.isNotEmpty) {
      final imgs = List<String>.of(_images);
      setState(_images.clear);
      _text.clear();
      ref
          .read(chatControllerProvider(widget.sessionId).notifier)
          .sendWithImages(t, imgs, model: ref.read(composerModelProvider));
      return;
    }
    if (t.trim().isEmpty) return;
    _text.clear();
    ref
        .read(chatControllerProvider(widget.sessionId).notifier)
        .send(
          widget.promptPrefix == null ? t : '${widget.promptPrefix!()}\n$t',
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
    ref.listen(composerDraftProvider, (_, d) {
      if (d == null || d.$1 != widget.sessionId) return;
      _text.text = d.$2;
      _text.selection = TextSelection.collapsed(offset: d.$2.length);
      _focus.requestFocus();
      ref.read(composerDraftProvider.notifier).clear();
    });
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
    final canSend = _text.text.trim().isNotEmpty || _images.isNotEmpty;
    Widget chip(IconData icon, String label, String tip, VoidCallback onTap) =>
        Tooltip(
          message: tip,
          child: InkWell(
            borderRadius: BorderRadius.circular(HkTokens.radiusChip),
            onTap: onTap,
            child: Container(
              height: 28,
              constraints: const BoxConstraints(maxWidth: 220),
              padding: const EdgeInsets.symmetric(horizontal: 10),
              decoration: BoxDecoration(
                color: hk.surface3,
                borderRadius: BorderRadius.circular(HkTokens.radiusChip),
              ),
              child: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  Icon(icon, size: 14, color: hk.textMuted),
                  const SizedBox(width: 6),
                  Flexible(
                    child: Text(
                      label,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(
                        fontSize: 12,
                        fontWeight: FontWeight.w500,
                      ),
                    ),
                  ),
                ],
              ),
            ),
          ),
        );
    // Own the bottom inset: home-indicator safe area when the composer sits at
    // the screen edge; the shell Scaffold already lifts it above the keyboard.
    return SafeArea(
      top: false,
      child: Padding(
        padding: EdgeInsets.fromLTRB(
          narrow ? HkTokens.s2 : HkTokens.s5,
          0,
          narrow ? HkTokens.s2 : HkTokens.s5,
          narrow ? HkTokens.s2 : HkTokens.s4,
        ),
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 900),
            child: Column(
              mainAxisSize: MainAxisSize.min,
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
                Container(
                  decoration: BoxDecoration(
                    color: hk.surface2,
                    borderRadius: BorderRadius.circular(16),
                    border: Border.all(
                      color: _focus.hasFocus ? hk.borderStrong : hk.border,
                    ),
                    boxShadow: [
                      BoxShadow(
                        color: Colors.black.withValues(alpha: 0.18),
                        blurRadius: 24,
                        offset: const Offset(0, 8),
                      ),
                    ],
                  ),
                  padding: const EdgeInsets.fromLTRB(6, 6, 8, 8),
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.stretch,
                    children: [
                      if (_images.isNotEmpty)
                        Padding(
                          padding: const EdgeInsets.fromLTRB(6, 4, 6, 0),
                          child: Wrap(
                            spacing: 6,
                            runSpacing: 6,
                            children: [
                              for (final p in List<String>.of(_images))
                                InputChip(
                                  avatar: const Icon(
                                    Icons.image_outlined,
                                    size: 16,
                                  ),
                                  label: Text(p.split(RegExp(r'[\\/]')).last),
                                  onDeleted: () =>
                                      setState(() => _images.remove(p)),
                                ),
                            ],
                          ),
                        ),
                      CallbackShortcuts(
                        bindings: bindings,
                        child: TextField(
                          controller: _text,
                          focusNode: _focus,
                          minLines: 1,
                          maxLines: 8,
                          textInputAction: TextInputAction.newline,
                          style: const TextStyle(fontSize: 14, height: 1.5),
                          decoration: InputDecoration(
                            filled: false,
                            border: InputBorder.none,
                            enabledBorder: InputBorder.none,
                            focusedBorder: InputBorder.none,
                            contentPadding: const EdgeInsets.fromLTRB(
                              10,
                              10,
                              10,
                              6,
                            ),
                            hintText: widget.running
                                ? 'Steer the running turn…'
                                : (fileMentionsSupported
                                      ? 'Message Hokkabaz  ·  / commands  ·  @ files'
                                      : 'Message Hokkabaz  ·  / commands'),
                          ),
                        ),
                      ),
                      Row(
                        children: [
                          IconButton(
                            tooltip: 'Attach image (vision models)',
                            icon: const Icon(
                              Icons.add_photo_alternate_outlined,
                            ),
                            onPressed: _attach,
                          ),
                          IconButton(
                            tooltip: _voice == 'recording'
                                ? 'Stop and transcribe'
                                : 'Voice input',
                            color: _voice == 'recording' ? hk.err : null,
                            icon: _voice == 'transcribing'
                                ? const SizedBox(
                                    width: 16,
                                    height: 16,
                                    child: CircularProgressIndicator(
                                      strokeWidth: 2,
                                    ),
                                  )
                                : Icon(
                                    _voice == 'recording'
                                        ? Icons.stop_circle
                                        : Icons.mic_none,
                                  ),
                            onPressed: _voice == 'transcribing'
                                ? null
                                : _toggleVoice,
                          ),
                          const SizedBox(width: 4),
                          Expanded(
                            child: Row(
                              children: [
                                chip(
                                  mode == ModeDto.plan
                                      ? Icons.map_outlined
                                      : Icons.build_outlined,
                                  mode == ModeDto.plan ? 'Plan' : 'Build',
                                  'Plan: read-only. Build: may edit and run tools.',
                                  () => ref
                                      .read(chatModeProvider.notifier)
                                      .set(
                                        mode == ModeDto.plan
                                            ? ModeDto.build
                                            : ModeDto.plan,
                                      ),
                                ),
                                const SizedBox(width: 6),
                                Flexible(
                                  child: chip(
                                    Icons.memory_outlined,
                                    _modelLabel(),
                                    'Choose model',
                                    () => showModelPicker(context),
                                  ),
                                ),
                              ],
                            ),
                          ),
                          AnimatedOpacity(
                            duration: const Duration(milliseconds: 150),
                            opacity: canSend ? 1 : 0.4,
                            child: Tooltip(
                              message: 'Send (Enter)',
                              child: InkWell(
                                customBorder: const CircleBorder(),
                                onTap: canSend ? _send : null,
                                child: Container(
                                  width: 34,
                                  height: 34,
                                  decoration: BoxDecoration(
                                    shape: BoxShape.circle,
                                    gradient: hk.gradient,
                                  ),
                                  child: Icon(
                                    Icons.arrow_upward_rounded,
                                    size: 19,
                                    color: hk.onAccent,
                                  ),
                                ),
                              ),
                            ),
                          ),
                        ],
                      ),
                    ],
                  ),
                ),
              ],
            ),
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
        color: hk.surface2,
        border: Border.all(color: hk.borderStrong),
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
