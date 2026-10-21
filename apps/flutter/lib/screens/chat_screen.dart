import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../ffi/ffi.dart';
import '../theme/tokens.dart';
import 'placeholder_screen.dart';

/// Chat tab placeholder; the debug line proves the Rust bridge is live (P0.5).
class ChatScreen extends ConsumerWidget {
  const ChatScreen({super.key, required this.title, required this.icon});

  final String title;
  final IconData icon;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final version = ref.watch(coreVersionProvider);
    final tick = ref.watch(demoTickerProvider);
    final style = Theme.of(context).textTheme.bodySmall
        ?.copyWith(color: context.hk.textMuted);
    return Column(
      children: [
        Expanded(
          child: PlaceholderScreen(title: title, icon: icon),
        ),
        Padding(
          padding: const EdgeInsets.all(HkTokens.s2),
          child: Text(
            'core ${version.isEmpty ? '-' : version} · tick ${tick.value ?? '-'}',
            style: style,
          ),
        ),
      ],
    );
  }
}
