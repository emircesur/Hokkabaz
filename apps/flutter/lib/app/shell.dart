import 'package:flutter/material.dart';
import 'package:go_router/go_router.dart';

import '../theme/tokens.dart';
import 'destinations.dart';

/// Desktop >= this width gets a rail; narrower gets bottom nav.
const double kRailBreakpoint = 720;

class AppShell extends StatelessWidget {
  const AppShell({super.key, required this.location, required this.child});

  final String location;
  final Widget child;

  int get _index {
    final i = destinations.indexWhere((d) => location.startsWith(d.path));
    return i < 0 ? 0 : i;
  }

  @override
  Widget build(BuildContext context) {
    final wide = MediaQuery.sizeOf(context).width >= kRailBreakpoint;
    return wide ? _wide(context) : _narrow(context);
  }

  Widget _wide(BuildContext context) {
    return Scaffold(
      body: Row(
        children: [
          SafeArea(
            child: NavigationRail(
              selectedIndex: _index,
              labelType: NavigationRailLabelType.all,
              onDestinationSelected: (i) => context.go(destinations[i].path),
              destinations: [
                for (final d in destinations)
                  NavigationRailDestination(
                    icon: Icon(d.icon),
                    selectedIcon: Icon(d.selectedIcon),
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

  Widget _narrow(BuildContext context) {
    final idx = _index;
    // Tabs beyond the primary set are reached via "More"; keep it highlighted.
    final selected = idx < mobilePrimaryCount ? idx : mobilePrimaryCount;
    final primary = destinations.take(mobilePrimaryCount).toList();
    return Scaffold(
      body: SafeArea(bottom: false, child: child),
      bottomNavigationBar: NavigationBar(
        selectedIndex: selected,
        onDestinationSelected: (i) {
          if (i < mobilePrimaryCount) {
            context.go(primary[i].path);
          } else {
            _showMore(context);
          }
        },
        destinations: [
          for (final d in primary)
            NavigationDestination(
              icon: Icon(d.icon),
              selectedIcon: Icon(d.selectedIcon),
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

  void _showMore(BuildContext context) {
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
          ],
        ),
      ),
    );
  }
}
