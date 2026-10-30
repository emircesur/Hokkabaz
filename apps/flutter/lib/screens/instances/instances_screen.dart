import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart';
import '../../src/rust/dto.dart';
import '../../theme/tokens.dart';
import 'instance_dialogs.dart';
import 'instances_providers.dart';

/// DESIGN §4.6 Instances: this device + remote `hokkabazd`s, health, switch.
class InstancesScreen extends ConsumerStatefulWidget {
  const InstancesScreen({super.key});

  @override
  ConsumerState<InstancesScreen> createState() => _InstancesScreenState();
}

class _InstancesScreenState extends ConsumerState<InstancesScreen> {
  Timer? _poll;

  @override
  void initState() {
    super.initState();
    _poll = Timer.periodic(const Duration(seconds: 20), (_) => _reprobe());
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }

  void _reprobe() {
    final list = ref.read(instanceBookProvider).value ?? const [];
    for (final i in list) {
      ref.invalidate(instanceStatusProvider(i.id));
    }
  }

  @override
  Widget build(BuildContext context) {
    final backendError = ref.watch(backendErrorProvider);
    final book = ref.watch(instanceBookProvider);
    final text = Theme.of(context).textTheme;
    final hk = context.hk;
    return Align(
      alignment: Alignment.topCenter,
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 800),
        child: ListView(
          padding: const EdgeInsets.all(HkTokens.s4),
          children: [
            Text('Instances', style: text.titleLarge),
            const SizedBox(height: HkTokens.s1),
            Text(
              'Run chats and flows on this device or on a remote hokkabazd.',
              style: text.bodySmall?.copyWith(color: hk.textMuted),
            ),
            const SizedBox(height: HkTokens.s3),
            Wrap(
              spacing: HkTokens.s2,
              runSpacing: HkTokens.s2,
              children: [
                FilledButton.icon(
                  icon: const Icon(Icons.add, size: 18),
                  label: const Text('Add instance'),
                  onPressed: backendError != null
                      ? null
                      : () => showAddInstanceDialog(context),
                ),
                OutlinedButton.icon(
                  icon: const Icon(Icons.link, size: 18),
                  label: const Text('Pair with link'),
                  onPressed: backendError != null
                      ? null
                      : () => showPairDialog(context),
                ),
              ],
            ),
            const SizedBox(height: HkTokens.s4),
            if (backendError != null)
              Text(backendError, style: TextStyle(color: hk.err))
            else
              book.when(
                loading: () => const Padding(
                  padding: EdgeInsets.all(HkTokens.s6),
                  child: Center(child: CircularProgressIndicator()),
                ),
                error: (e, _) => Text('$e', style: TextStyle(color: hk.err)),
                data: (list) => Column(
                  children: [
                    for (final i in list)
                      Padding(
                        padding: const EdgeInsets.only(bottom: HkTokens.s2),
                        child: _InstanceCard(instance: i),
                      ),
                  ],
                ),
              ),
          ],
        ),
      ),
    );
  }
}

class _InstanceCard extends ConsumerStatefulWidget {
  const _InstanceCard({required this.instance});
  final InstanceDto instance;

  @override
  ConsumerState<_InstanceCard> createState() => _InstanceCardState();
}

enum _Menu { makeDefault, recheck, remove }

class _InstanceCardState extends ConsumerState<_InstanceCard> {
  bool _busy = false;

  Future<void> _act(Future<String?> Function() op, {String? ok}) async {
    setState(() => _busy = true);
    final err = await op();
    if (!mounted) return;
    setState(() => _busy = false);
    ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(err ?? ok ?? 'Done')));
    ref.invalidate(instanceStatusProvider(widget.instance.id));
  }

  Future<void> _confirmRemove() async {
    final i = widget.instance;
    final yes = await showDialog<bool>(
      context: context,
      builder: (ctx) => AlertDialog(
        title: Text('Remove ${i.name}?'),
        content: const Text(
          'The stored token is deleted from this device. '
          'The instance itself is not touched.',
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.of(ctx).pop(false),
            child: const Text('Cancel'),
          ),
          FilledButton(
            onPressed: () => Navigator.of(ctx).pop(true),
            child: const Text('Remove'),
          ),
        ],
      ),
    );
    if (yes == true) {
      await _act(
        () => ref.read(instanceBookProvider.notifier).remove(i.id),
        ok: 'Removed',
      );
    }
  }

  @override
  Widget build(BuildContext context) {
    final i = widget.instance;
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final book = ref.read(instanceBookProvider.notifier);
    final status = ref.watch(instanceStatusProvider(i.id));
    final (dot, detail) = status.when(
      loading: () => (hk.textMuted, 'Checking…'),
      error: (e, _) => (hk.err, '$e'),
      data: (s) => switch (s.state) {
        InstanceStateDto.online => (
          hk.ok,
          [
            if (s.version != null) 'v${s.version}',
            if (s.latencyMs != null && i.kind == InstanceKindDto.remote)
              '${s.latencyMs} ms',
          ].join(' · '),
        ),
        InstanceStateDto.unauthorized => (
          hk.warn,
          s.message ?? 'Token rejected',
        ),
        InstanceStateDto.unreachable => (hk.err, s.message ?? 'Unreachable'),
      },
    );
    return Container(
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: i.connected ? hk.accent : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Padding(
            padding: const EdgeInsets.only(top: 6, right: HkTokens.s3),
            child: Semantics(
              label: 'status',
              child: Container(
                width: 10,
                height: 10,
                decoration: BoxDecoration(color: dot, shape: BoxShape.circle),
              ),
            ),
          ),
          Expanded(
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Wrap(
                  spacing: HkTokens.s2,
                  runSpacing: HkTokens.s1,
                  crossAxisAlignment: WrapCrossAlignment.center,
                  children: [
                    Text(i.name, style: text.titleSmall),
                    if (i.connected) _Chip('Connected', hk.accent),
                    if (i.isDefault && !i.connected)
                      _Chip('Default', hk.textMuted),
                  ],
                ),
                if (i.baseUrl != null)
                  Text(
                    i.baseUrl!,
                    style: text.bodySmall?.copyWith(
                      color: hk.textMuted,
                      fontFamily: HkTokens.fontMono,
                    ),
                    overflow: TextOverflow.ellipsis,
                  ),
                if (detail.isNotEmpty)
                  Text(
                    detail,
                    style: text.bodySmall?.copyWith(color: hk.textMuted),
                  ),
              ],
            ),
          ),
          const SizedBox(width: HkTokens.s2),
          if (_busy)
            const Padding(
              padding: EdgeInsets.all(HkTokens.s2),
              child: SizedBox(
                width: 20,
                height: 20,
                child: CircularProgressIndicator(strokeWidth: 2),
              ),
            )
          else ...[
            if (!i.connected)
              FilledButton.tonal(
                onPressed: () => _act(
                  () => book.connect(i.id),
                  ok: 'Connected to ${i.name}',
                ),
                child: const Text('Connect'),
              ),
            PopupMenuButton<_Menu>(
              tooltip: 'More',
              onSelected: (m) {
                switch (m) {
                  case _Menu.makeDefault:
                    _act(() => book.setDefault(i.id), ok: 'Default set');
                  case _Menu.recheck:
                    ref.invalidate(instanceStatusProvider(i.id));
                  case _Menu.remove:
                    _confirmRemove();
                }
              },
              itemBuilder: (_) => [
                if (!i.isDefault)
                  const PopupMenuItem(
                    value: _Menu.makeDefault,
                    child: Text('Use at startup'),
                  ),
                const PopupMenuItem(
                  value: _Menu.recheck,
                  child: Text('Check status'),
                ),
                if (i.kind == InstanceKindDto.remote)
                  const PopupMenuItem(
                    value: _Menu.remove,
                    child: Text('Remove'),
                  ),
              ],
            ),
          ],
        ],
      ),
    );
  }
}

class _Chip extends StatelessWidget {
  const _Chip(this.label, this.color);
  final String label;
  final Color color;

  @override
  Widget build(BuildContext context) {
    return Container(
      padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2, vertical: 2),
      decoration: BoxDecoration(
        border: Border.all(color: color),
        borderRadius: BorderRadius.circular(HkTokens.radiusChip),
      ),
      child: Text(
        label,
        style: Theme.of(context).textTheme.labelSmall?.copyWith(color: color),
      ),
    );
  }
}
