import 'dart:async';
import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import 'studio_api.dart';

const _channel = MethodChannel('hokkabaz/concat_editor');

Future<void> setConcatVisibility(bool visible) async {
  if (!Platform.isWindows && !Platform.isLinux) return;
  await _channel.invokeMethod<void>('visible', {'visible': visible});
}

/// Native Concat retains its complete UI and live compositor. Flutter owns the
/// surrounding workspace; native pixels occupy only this widget's rectangle.
class ConcatEditor extends StatefulWidget {
  const ConcatEditor({super.key, required this.projectId, this.clipId});
  final String projectId;
  final String? clipId;
  @override
  State<ConcatEditor> createState() => _ConcatEditorState();
}

class _ConcatEditorState extends State<ConcatEditor>
    with WidgetsBindingObserver {
  final _surface = GlobalKey();
  Timer? _timer;
  Json? _editor;
  String? _error;
  bool _starting = true;
  bool _running = false;
  bool _embedded = false;
  bool _checking = false;
  bool _foreground = true;
  Rect? _bounds;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    unawaited(_start());
    _timer = Timer.periodic(const Duration(milliseconds: 250), (_) => _tick());
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _foreground = state == AppLifecycleState.resumed;
    unawaited(_visible(_foreground));
  }

  Future<void> _visible(bool value) async {
    try {
      await _channel.invokeMethod<void>('visible', {'visible': value});
    } on PlatformException catch (_) {
      /* Startup reports platform errors. */
    } on MissingPluginException catch (_) {
      /* Unsupported desktop. */
    }
  }

  Future<void> _start({bool rebuild = false}) async {
    setState(() {
      _starting = true;
      _error = null;
    });
    try {
      if (!Platform.isWindows && !Platform.isLinux) {
        throw StateError(
          'The embedded editor is available on Windows and Linux X11.',
        );
      }
      if (rebuild && _running) {
        throw StateError(
          'Save and close the current editor before applying Shorts edits. Your existing edit is kept as a separate project.',
        );
      }
      final editor = object(
        await studioRequest('prepare_editor', {
          'project_id': widget.projectId,
          'clip_id': widget.clipId,
          'rebuild': rebuild,
        }),
      );
      if (!mounted) return;
      if (!await File(editor['binary'] as String).exists() ||
          !await File('${editor['path']}/concat.json').exists()) {
        throw StateError(
          'Select the local instance to use the embedded editor. The project and Concat must be on this computer.',
        );
      }
      await _channel.invokeMethod<void>('start', editor);
      if (!mounted) {
        await _visible(false);
        return;
      }
      setState(() => _editor = editor);
      _bounds = null;
      _updateBounds();
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _starting = false);
    }
  }

  Future<void> _tick() async {
    if (_checking || !mounted || _starting) return;
    _checking = true;
    try {
      _updateBounds();
      final data = await _channel.invokeMapMethod<String, dynamic>('status');
      if (!mounted || data == null) return;
      final error = data['error'] as String?;
      final running = data['running'] == true;
      final embedded = data['embedded'] == true;
      if (running != _running ||
          embedded != _embedded ||
          (error?.isNotEmpty == true && error != _error)) {
        setState(() {
          _running = running;
          _embedded = embedded;
          if (error?.isNotEmpty == true) _error = error;
        });
      }
    } on PlatformException catch (error) {
      if (mounted) setState(() => _error = error.message);
    } on MissingPluginException catch (_) {
      // An unsupported platform is already explained by _start.
    } finally {
      _checking = false;
    }
  }

  void _updateBounds() {
    if (!Platform.isWindows && !Platform.isLinux) return;
    final render = _surface.currentContext?.findRenderObject();
    if (render is! RenderBox || !render.hasSize) return;
    final rect = render.localToGlobal(Offset.zero) & render.size;
    if (rect == _bounds) return;
    _bounds = rect;
    unawaited(
      _channel.invokeMethod<void>('bounds', {
        'x': rect.left,
        'y': rect.top,
        'width': rect.width,
        'height': rect.height,
      }),
    );
    unawaited(_visible(_foreground));
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _timer?.cancel();
    // Keep unsaved native edits alive while navigating to Shorts or Chat.
    unawaited(_visible(false));
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    children: [
      Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 6),
        child: Wrap(
          spacing: 12,
          crossAxisAlignment: WrapCrossAlignment.center,
          children: [
            const Icon(Icons.movie_edit),
            Text(_embedded ? 'Concat · live editor' : 'Concat · full editor'),
            TextButton.icon(
              onPressed: _embedded
                  ? () => _channel.invokeMethod<void>('save')
                  : null,
              icon: const Icon(Icons.save_outlined),
              label: const Text('Save · Ctrl+S'),
            ),
            TextButton(
              onPressed: _running
                  ? () => _channel.invokeMethod<void>('close')
                  : null,
              child: const Text('Close project'),
            ),
            TextButton(
              onPressed: !_starting && !_running ? () => _start() : null,
              child: const Text('Reopen'),
            ),
            TextButton(
              onPressed: !_starting && !_running
                  ? () => _start(rebuild: true)
                  : null,
              child: const Text('Apply latest Shorts edits'),
            ),
            if (_editor?['shorts_changed'] == true)
              const Text('Shorts changed · close, then apply latest edits'),
          ],
        ),
      ),
      if (_error != null)
        Padding(
          padding: const EdgeInsets.all(8),
          child: Text(
            _error!,
            style: TextStyle(color: Theme.of(context).colorScheme.error),
          ),
        ),
      Expanded(
        child: Container(
          key: _surface,
          color: const Color(0xff18181c),
          child: Center(
            child: _embedded
                ? const SizedBox.shrink()
                : _starting || _running
                ? const Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      CircularProgressIndicator(),
                      SizedBox(height: 12),
                      Text('Starting full Concat editor…'),
                    ],
                  )
                : const Text('Your edit is kept on disk. Reopen to continue.'),
          ),
        ),
      ),
    ],
  );
}
