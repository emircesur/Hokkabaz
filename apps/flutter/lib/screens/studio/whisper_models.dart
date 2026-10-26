import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';

import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;

/// Pick, download and delete the speech models behind "Local whisper.cpp". Downloads run where the
/// Studio runs (this device, or the connected instance); the list polls while one is in flight.
class WhisperModelsPicker extends StatefulWidget {
  const WhisperModelsPicker({
    super.key,
    required this.selected,
    required this.onSelected,
  });
  final String? selected;
  final ValueChanged<String> onSelected;

  @override
  State<WhisperModelsPicker> createState() => _WhisperModelsPickerState();
}

class _WhisperModelsPickerState extends State<WhisperModelsPicker> {
  bool _helper = true;
  List<Map<String, dynamic>> _models = const [];
  String? _error;
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  Future<void> _refresh() async {
    try {
      final j =
          jsonDecode(await api.studioWhisperStatus()) as Map<String, dynamic>;
      if (!mounted) return;
      final models = (j['models'] as List).cast<Map<String, dynamic>>();
      setState(() {
        _helper = j['helper'] == true;
        _models = models;
        _error = null;
      });
      final busy = models.any((m) => m['downloading'] == true);
      _poll?.cancel();
      if (busy) _poll = Timer(const Duration(seconds: 1), _refresh);
    } catch (e) {
      if (mounted) setState(() => _error = describeError(e));
    }
  }

  Future<void> _do(Future<void> Function() f) async {
    try {
      await f();
    } catch (e) {
      if (mounted) setState(() => _error = describeError(e));
    }
    await _refresh();
  }

  static String _mb(num b) => '${(b / 1000000).round()} MB';

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final sel = widget.selected ?? 'base';
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        if (!_helper)
          Container(
            padding: const EdgeInsets.all(HkTokens.s2),
            margin: const EdgeInsets.only(bottom: HkTokens.s2),
            decoration: BoxDecoration(
              color: hk.warn.withValues(alpha: 0.15),
              borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            ),
            child: const Text(
              'This install has no hk-whisper helper, so it cannot transcribe on its own. Use "API / server", or run Studio on an instance that has it.',
            ),
          ),
        for (final m in _models)
          RadioListTile<String>(
            dense: true,
            contentPadding: EdgeInsets.zero,
            value: m['id'] as String,
            // ignore: deprecated_member_use
            groupValue: sel,
            // ignore: deprecated_member_use
            onChanged: (v) => widget.onSelected(v!),
            title: Text('${m['name']}  ·  ${_mb(m['size_bytes'] as num)}'),
            subtitle: m['downloading'] == true
                ? _progress(m)
                : Text(
                    (m['error'] as String?) ?? (m['note'] as String),
                    style: TextStyle(
                      color: m['error'] != null ? hk.err : hk.textMuted,
                    ),
                  ),
            secondary: _action(m),
          ),
        if (_error != null) Text(_error!, style: TextStyle(color: hk.err)),
      ],
    );
  }

  Widget _progress(Map<String, dynamic> m) {
    final p = m['progress'] as Map<String, dynamic>?;
    final total = (p?['total'] as num?) ?? 0;
    final done = (p?['done'] as num?) ?? 0;
    return LinearProgressIndicator(
      value: total > 0 ? (done / total).toDouble() : null,
    );
  }

  Widget _action(Map<String, dynamic> m) {
    final id = m['id'] as String;
    if (m['downloading'] == true) {
      return IconButton(
        tooltip: 'Cancel',
        icon: const Icon(Icons.close),
        onPressed: () => _do(() => api.studioWhisperCancel(id: id)),
      );
    }
    if (m['installed'] == true) {
      return IconButton(
        tooltip: 'Delete',
        icon: const Icon(Icons.delete_outline),
        onPressed: () => _do(() => api.studioWhisperRemove(id: id)),
      );
    }
    return IconButton(
      tooltip: 'Download',
      icon: const Icon(Icons.download),
      onPressed: () => _do(() => api.studioWhisperPull(id: id)),
    );
  }
}
