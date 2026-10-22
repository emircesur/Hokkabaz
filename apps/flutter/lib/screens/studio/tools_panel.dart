import 'dart:async';
import 'dart:convert';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../src/rust/api/studio.dart' as api;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;
import 'studio_providers.dart';

/// What the Studio needs and whether it is there (ffmpeg, the Concat editor), with one-tap
/// downloads. Polls while an install runs; on a connected instance it manages the instance's tools.
class ToolsPanel extends ConsumerStatefulWidget {
  const ToolsPanel({super.key, this.onlyMissing = false});

  /// Show only tools that are not installed (the banner on the Studio screen).
  final bool onlyMissing;

  @override
  ConsumerState<ToolsPanel> createState() => _ToolsPanelState();
}

class _ToolsPanelState extends ConsumerState<ToolsPanel> {
  List<Map<String, dynamic>> _tools = const [];
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
      final j = jsonDecode(await api.studioDepsStatus()) as Map<String, dynamic>;
      if (!mounted) return;
      final tools = (j['tools'] as List).cast<Map<String, dynamic>>();
      final wasBusy = _tools.any((t) => t['installing'] == true);
      setState(() {
        _tools = tools;
        _error = null;
      });
      final busy = tools.any((t) => t['installing'] == true);
      _poll?.cancel();
      if (busy) _poll = Timer(const Duration(milliseconds: 800), _refresh);
      // ffmpeg arrived: the Studio screen's warning goes away without a restart
      if (wasBusy && !busy) ref.invalidate(studioToolsProvider);
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

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final shown = [for (final t in _tools) if (!widget.onlyMissing || t['installed'] != true) t];
    if (shown.isEmpty && _error == null) return const SizedBox.shrink();
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        for (final t in shown)
          Container(
            margin: const EdgeInsets.only(bottom: HkTokens.s2),
            padding: const EdgeInsets.all(HkTokens.s3),
            decoration: BoxDecoration(
              color: t['installed'] == true ? hk.surface : hk.warn.withValues(alpha: 0.12),
              border: Border.all(color: hk.border),
              borderRadius: BorderRadius.circular(HkTokens.radiusCard),
            ),
            child: Row(
              children: [
                Icon(t['installed'] == true ? Icons.check_circle_outline : Icons.download_for_offline_outlined, color: t['installed'] == true ? hk.ok : hk.warn),
                const SizedBox(width: HkTokens.s3),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text('${t['name']}${t['version'] != null ? '  ·  ${t['version']}' : ''}', style: text.titleSmall, maxLines: 1, overflow: TextOverflow.ellipsis),
                      const SizedBox(height: 2),
                      if (t['installing'] == true) ...[
                        Text('${t['phase'] ?? 'working'}…', style: text.bodySmall),
                        const SizedBox(height: 4),
                        _bar(t['progress']),
                      ] else
                        Text((t['error'] as String?) ?? (t['note'] as String), style: text.bodySmall?.copyWith(color: t['error'] != null ? hk.err : hk.textMuted)),
                    ],
                  ),
                ),
                const SizedBox(width: HkTokens.s2),
                if (t['installing'] == true)
                  TextButton(onPressed: () => _do(() => api.studioDepsCancel(id: t['id'] as String)), child: const Text('Cancel'))
                else if (t['installed'] != true && t['can_install'] == true)
                  FilledButton(onPressed: () => _do(() => api.studioDepsInstall(id: t['id'] as String)), child: const Text('Install'))
                else if (t['installed'] == true)
                  IconButton(tooltip: 'Check again', onPressed: _refresh, icon: const Icon(Icons.refresh, size: 18)),
              ],
            ),
          ),
        if (_error != null) Text(_error!, style: TextStyle(color: hk.err)),
      ],
    );
  }

  Widget _bar(Object? progress) {
    final p = progress as List?;
    final total = p == null ? 0 : (p[1] as num);
    final done = p == null ? 0 : (p[0] as num);
    return LinearProgressIndicator(value: total > 0 ? (done / total).toDouble() : null);
  }
}
