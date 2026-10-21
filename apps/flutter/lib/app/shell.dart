import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../ffi/notifications.dart';
import '../screens/chat/chat_providers.dart';
import '../theme/tokens.dart';
import 'breakpoints.dart';
import 'command_palette.dart';
import 'destinations.dart';
import 'theme_mode.dart';

class AppShell extends ConsumerWidget {
  const AppShell({super.key, required this.location, required this.child});

  final String location;
  final Widget child;

  int get _index {
    final i = destinations.indexWhere((d) => location.startsWith(d.path));
    return i < 0 ? 0 : i;
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final wide = !isNarrow(context);
    return AppShortcuts(
      child: wide ? _wide(context, ref) : _narrow(context, ref),
    );
  }

  Widget _wide(BuildContext context, WidgetRef ref) {
    final theme = ref.watch(themeModeProvider);
    final pending = ref
        .watch(approvalInboxProvider)
        .values
        .fold<int>(0, (n, l) => n + l.length);
    return Scaffold(
      body: Row(
        children: [
          SafeArea(
            child: NavigationRail(
              selectedIndex: _index,
              labelType: NavigationRailLabelType.all,
              trailing: Expanded(
                child: Align(
                  alignment: Alignment.bottomCenter,
                  child: Padding(
                    padding: const EdgeInsets.only(bottom: HkTokens.s3),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        IconButton(
                          tooltip: 'Command palette (Ctrl+K)',
                          icon: const Icon(Icons.keyboard_command_key),
                          onPressed: () => openCommandPalette(context, ref),
                        ),
                        IconButton(
                          tooltip: 'Theme: ${themeModeLabel(theme)}',
                          icon: Icon(themeModeIcon(theme)),
                          onPressed: () =>
                              ref.read(themeModeProvider.notifier).cycle(),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
              onDestinationSelected: (i) => context.go(destinations[i].path),
              destinations: [
                for (final d in destinations)
                  NavigationRailDestination(
                    icon: _badged(d.icon, d.path, pending),
                    selectedIcon: _badged(d.selectedIcon, d.path, pending),
                    label: Text(d.label),
                  ),
              ],
            ),
          ),
          VerticalDivider(color: context.hk.border),
          Expanded(child: child),
        ],
      ),
    );
  }

  /// Approval count badge on the Chat destination.
  Widget _badged(IconData icon, String path, int pending) {
    if (path != '/chat' || pending == 0) return Icon(icon);
    return Badge.count(count: pending, child: Icon(icon));
  }

  Widget _narrow(BuildContext context, WidgetRef ref) {
    final idx = _index;
    // Tabs beyond the primary set are reached via "More"; keep it highlighted.
    final selected = idx < mobilePrimaryCount ? idx : mobilePrimaryCount;
    final primary = destinations.take(mobilePrimaryCount).toList();
    final openSession = ref.watch(selectedSessionProvider);
    final pending = ref
        .watch(approvalInboxProvider)
        .values
        .fold<List<ApprovalNotice>>([], (a, l) => [...a, ...l]);
    // A conversation takes the whole screen (back arrow returns to the list).
    final fullScreen = location.startsWith('/chat') && openSession != null;
    // Approvals for the session on screen show inline; surface the others.
    final elsewhere = [
      for (final n in pending)
        if (!fullScreen || n.sessionId != openSession) n,
    ];
    return Scaffold(
      resizeToAvoidBottomInset: true,
      body: SafeArea(
        bottom: false,
        child: Column(
          children: [
            if (elsewhere.isNotEmpty)
              _ApprovalBanner(
                notice: elsewhere.first,
                total: elsewhere.length,
                onTap: () {
                  ref
                      .read(selectedSessionProvider.notifier)
                      .select(elsewhere.first.sessionId);
                  context.go('/chat');
                },
              ),
            Expanded(child: child),
          ],
        ),
      ),
      bottomNavigationBar: fullScreen
          ? null
          : NavigationBar(
              selectedIndex: selected,
              onDestinationSelected: (i) {
                if (i < mobilePrimaryCount) {
                  context.go(primary[i].path);
                } else {
                  _showMore(context, ref);
                }
              },
              destinations: [
                for (final d in primary)
                  NavigationDestination(
                    icon: _badged(d.icon, d.path, pending.length),
                    selectedIcon: _badged(
                      d.selectedIcon,
                      d.path,
                      pending.length,
                    ),
                    label: d.label,
                  ),
                const NavigationDestination(
                  icon: Icon(Icons.more_horiz),
                  label: 'More',
                ),
              ],
            ),
    );
  }

  void _showMore(BuildContext context, WidgetRef ref) {
    final rest = destinations.skip(mobilePrimaryCount).toList();
    showModalBottomSheet<void>(
      context: context,
      backgroundColor: context.hk.surface,
      shape: const RoundedRectangleBorder(
        borderRadius: BorderRadius.vertical(
          top: Radius.circular(HkTokens.radiusSheet),
        ),
      ),
      builder: (sheetContext) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            for (final d in rest)
              ListTile(
                minTileHeight: 48,
                leading: Icon(d.icon),
                title: Text(d.label),
                onTap: () {
                  Navigator.of(sheetContext).pop();
                  context.go(d.path);
                },
              ),
            Consumer(
              builder: (_, ref, _) {
                final theme = ref.watch(themeModeProvider);
                return ListTile(
                  minTileHeight: 48,
                  leading: Icon(themeModeIcon(theme)),
                  title: Text('Theme: ${themeModeLabel(theme)}'),
                  onTap: () => ref.read(themeModeProvider.notifier).cycle(),
                );
              },
            ),
          ],
        ),
      ),
    );
  }
}

class _ApprovalBanner extends StatelessWidget {
  const _ApprovalBanner({
    required this.notice,
    required this.total,
    required this.onTap,
  });

  final ApprovalNotice notice;
  final int total;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Material(
      color: hk.warn.withValues(alpha: 0.15),
      child: InkWell(
        onTap: onTap,
        child: ConstrainedBox(
          constraints: const BoxConstraints(minHeight: kTouchTarget),
          child: Padding(
            padding: const EdgeInsets.symmetric(horizontal: HkTokens.s4),
            child: Row(
              children: [
                Icon(Icons.shield_outlined, size: 18, color: hk.warn),
                const SizedBox(width: HkTokens.s3),
                Expanded(
                  child: Text(
                    total > 1
                        ? 'Approval needed: ${notice.tool} (+${total - 1} more)'
                        : 'Approval needed: ${notice.tool}',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontWeight: FontWeight.w600),
                  ),
                ),
                Text('Review', style: TextStyle(color: hk.warn)),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
