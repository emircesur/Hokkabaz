import 'package:flutter/material.dart';

/// Tabs in DESIGN.md §4.2 order. First [mobilePrimaryCount] go in the bottom
/// bar; the rest live under "More".
class HkDestination {
  const HkDestination(this.label, this.path, this.icon, this.selectedIcon);

  final String label;
  final String path;
  final IconData icon;
  final IconData selectedIcon;
}

const destinations = <HkDestination>[
  HkDestination('Chat', '/chat', Icons.chat_bubble_outline, Icons.chat_bubble),
  HkDestination(
    'Studio',
    '/studio',
    Icons.movie_creation_outlined,
    Icons.movie_creation,
  ),
  HkDestination(
    'Flows',
    '/flows',
    Icons.account_tree_outlined,
    Icons.account_tree,
  ),
  HkDestination('Runs', '/runs', Icons.play_circle_outline, Icons.play_circle),
  HkDestination('Models', '/models', Icons.memory_outlined, Icons.memory),
  HkDestination(
    'Plugins',
    '/plugins',
    Icons.extension_outlined,
    Icons.extension,
  ),
  HkDestination('Instances', '/instances', Icons.dns_outlined, Icons.dns),
  HkDestination(
    'Settings',
    '/settings',
    Icons.settings_outlined,
    Icons.settings,
  ),
];

/// Bottom bar shows 4 tabs + More (DESIGN §4.2).
const mobilePrimaryCount = 4;
