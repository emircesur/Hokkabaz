import 'package:flutter/material.dart';

import '../theme/tokens.dart';

/// Empty tab body until the real screen lands.
class PlaceholderScreen extends StatelessWidget {
  const PlaceholderScreen({super.key, required this.title, required this.icon});

  final String title;
  final IconData icon;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    return Center(
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 40, color: hk.textMuted),
          const SizedBox(height: HkTokens.s3),
          Text(title, style: text.titleLarge),
          const SizedBox(height: HkTokens.s1),
          Text('Nothing here yet', style: text.bodySmall),
        ],
      ),
    );
  }
}
