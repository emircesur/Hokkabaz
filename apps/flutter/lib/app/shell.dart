import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../ffi/android_runtime.dart';
import '../ffi/notifications.dart';
import '../screens/chat/approvals_inbox.dart';
import '../screens/chat/chat_providers.dart';
import '../theme/tokens.dart';
import 'activity.dart';
import 'notifications.dart';
import 'breakpoints.dart';
import 'command_palette.dart';
import 'destinations.dart';
import 'theme_mode.dart';
import '../widgets/hk_ui.dart';

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
    ref.listen(keepAliveProvider, (prev, next) {
      final hint = next.hint;
      if (hint != null && hint != prev?.hint) {
        ScaffoldMessenger.maybeOf(context)
          ?..hideCurrentSnackBar()
          ..showSnackBar(SnackBar(content: Text(hint)));
      }
    });
    // Long jobs finishing while the user is elsewhere (DESIGN §4.8).
    JobNotifications.instance.onCancel ??= (id) =>
        ref.read(jobsProvider.notifier).cancel(id);
    ref.listen(jobsProvider, (prev, next) {
      final now = DateTime.now().millisecondsSinceEpoch;
      for (final j in next.values) {
        if (j.parent == null && j.running && now - j.startedMs > 5000) {
          JobNotifications.instance.progress(j);
        }
      }
      for (final j in next.values) {
        final before = prev?[j.id];
        if (j.parent != null ||
            before == null ||
            !before.running ||
            j.running) {
          continue;
        }
        if (DateTime.now().millisecondsSinceEpoch - j.startedMs < 10000) {
          continue;
        }
        final msg = switch (j.state) {
          'finished' => 'Done: ${j.title}',
          'failed' => 'Failed: ${j.title}',
          _ => 'Cancelled: ${j.title}',
        };
        JobNotifications.instance.finished(j, msg);
        ScaffoldMessenger.maybeOf(context)?.showSnackBar(
          SnackBar(
            content: Text(msg),
            action: SnackBarAction(
              label: 'Activity',
              onPressed: () => showActivity(context),
            ),
          ),
        );
      }
    });
    final wide = !isNarrow(context);
    return AppShortcuts(
      child: wide ? _wide(context, ref) : _narrow(context, ref),
    );
  }

  Widget _wide(BuildContext context, WidgetRef ref) {
    final pending = ref
        .watch(approvalInboxProvider)
        .values
        .fold<int>(0, (n, l) => n + l.length);
    return Scaffold(
      body: Column(
        children: [
          _TopBar(title: destinations[_index].label),
          Expanded(
            child: Row(
              children: [
                _Rail(
                  index: _index,
                  pending: pending,
                  onSelect: (i) => context.go(destinations[i].path),
                ),
                Expanded(child: child),
              ],
            ),
          ),
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
                  if (elsewhere.length > 1) {
                    showApprovalsInbox(context);
                    return;
                  }
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
            ListTile(
              minTileHeight: 48,
              leading: const Icon(Icons.timelapse),
              title: const Text('Activity'),
              onTap: () {
                Navigator.of(sheetContext).pop();
                showActivity(context);
              },
            ),
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

/// Editor-style title bar: brand, current workspace, command search, global actions.
class _TopBar extends ConsumerWidget {
  const _TopBar({required this.title});

  final String title;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hk = context.hk;
    final theme = ref.watch(themeModeProvider);
    return Container(
      height: 48,
      padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(bottom: BorderSide(color: hk.border)),
      ),
      child: Row(
        children: [
          const BrandMark(size: 26),
          const SizedBox(width: HkTokens.s3),
          const Text(
            'Hokkabaz',
            style: TextStyle(
              fontSize: 14,
              fontWeight: FontWeight.w700,
              letterSpacing: -0.2,
            ),
          ),
          Padding(
            padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
            child: Icon(Icons.chevron_right, size: 16, color: hk.textFaint),
          ),
          Text(title, style: TextStyle(fontSize: 13, color: hk.textMuted)),
          const Spacer(),
          _SearchBox(onTap: () => openCommandPalette(context, ref)),
          const Spacer(),
          const ActivityButton(),
          IconButton(
            tooltip: 'Theme: ${themeModeLabel(theme)}',
            icon: Icon(themeModeIcon(theme)),
            onPressed: () => ref.read(themeModeProvider.notifier).cycle(),
          ),
          IconButton(
            tooltip: 'Settings',
            icon: const Icon(Icons.tune),
            onPressed: () => context.go('/settings'),
          ),
        ],
      ),
    );
  }
}

class _SearchBox extends StatelessWidget {
  const _SearchBox({required this.onTap});

  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return InkWell(
      onTap: onTap,
      borderRadius: BorderRadius.circular(HkTokens.radiusControl),
      child: Container(
        width: 420,
        height: 32,
        padding: const EdgeInsets.symmetric(horizontal: HkTokens.s3),
        decoration: BoxDecoration(
          color: hk.surface2,
          borderRadius: BorderRadius.circular(HkTokens.radiusControl),
          border: Border.all(color: hk.border),
        ),
        child: Row(
          children: [
            Icon(Icons.search, size: 16, color: hk.textFaint),
            const SizedBox(width: HkTokens.s2),
            Expanded(
              child: Text(
                'Search commands, chats, flows…',
                style: TextStyle(fontSize: 13, color: hk.textFaint),
              ),
            ),
            Container(
              padding: const EdgeInsets.symmetric(horizontal: 6, vertical: 2),
              decoration: BoxDecoration(
                color: hk.surface3,
                borderRadius: BorderRadius.circular(4),
              ),
              child: Text(
                'Ctrl K',
                style: TextStyle(
                  fontSize: 10,
                  fontWeight: FontWeight.w600,
                  color: hk.textMuted,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Slim icon rail with labels under icons and an accent bar on the active workspace.
class _Rail extends StatelessWidget {
  const _Rail({
    required this.index,
    required this.pending,
    required this.onSelect,
  });

  final int index;
  final int pending;
  final ValueChanged<int> onSelect;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      width: 72,
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border(right: BorderSide(color: hk.border)),
      ),
      child: ListView(
        padding: const EdgeInsets.symmetric(vertical: HkTokens.s2),
        children: [
          for (var i = 0; i < destinations.length; i++)
            _RailItem(
              d: destinations[i],
              selected: i == index,
              badge: destinations[i].path == '/chat' ? pending : 0,
              onTap: () => onSelect(i),
            ),
        ],
      ),
    );
  }
}

class _RailItem extends StatefulWidget {
  const _RailItem({
    required this.d,
    required this.selected,
    required this.badge,
    required this.onTap,
  });

  final HkDestination d;
  final bool selected;
  final int badge;
  final VoidCallback onTap;

  @override
  State<_RailItem> createState() => _RailItemState();
}

class _RailItemState extends State<_RailItem> {
  bool _hover = false;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final sel = widget.selected;
    Widget icon = Icon(
      sel ? widget.d.selectedIcon : widget.d.icon,
      size: 21,
      color: sel ? hk.text : (_hover ? hk.text : hk.textMuted),
    );
    if (widget.badge > 0) icon = Badge.count(count: widget.badge, child: icon);
    return MouseRegion(
      onEnter: (_) => setState(() => _hover = true),
      onExit: (_) => setState(() => _hover = false),
      cursor: SystemMouseCursors.click,
      child: GestureDetector(
        onTap: widget.onTap,
        child: SizedBox(
          height: 60,
          child: Stack(
            children: [
              if (sel)
                Positioned(
                  left: 0,
                  top: 14,
                  bottom: 14,
                  child: Container(
                    width: 3,
                    decoration: BoxDecoration(
                      gradient: hk.gradient,
                      borderRadius: const BorderRadius.horizontal(
                        right: Radius.circular(3),
                      ),
                    ),
                  ),
                ),
              Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    AnimatedContainer(
                      duration: const Duration(milliseconds: 140),
                      width: 40,
                      height: 32,
                      decoration: BoxDecoration(
                        color: sel
                            ? hk.accent.withValues(alpha: 0.18)
                            : (_hover ? hk.surface3 : Colors.transparent),
                        borderRadius: BorderRadius.circular(9),
                      ),
                      child: Center(child: icon),
                    ),
                    const SizedBox(height: 3),
                    Text(
                      widget.d.label,
                      style: TextStyle(
                        fontSize: 10.5,
                        fontWeight: sel ? FontWeight.w600 : FontWeight.w500,
                        color: sel ? hk.text : hk.textMuted,
                      ),
                    ),
                  ],
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
