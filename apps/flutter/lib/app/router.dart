import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:go_router/go_router.dart';

import '../screens/chat_screen.dart';
import '../screens/flows/flows_screen.dart';
import '../screens/instances/instances_screen.dart';
import '../screens/models/models_screen.dart';
import '../screens/placeholder_screen.dart';
import '../screens/runs/runs_screen.dart';
import 'destinations.dart';
import 'shell.dart';

final routerProvider = Provider<GoRouter>((ref) {
  return GoRouter(
    initialLocation: '/chat',
    routes: [
      ShellRoute(
        builder: (context, state, child) =>
            AppShell(location: state.uri.path, child: child),
        routes: [
          for (final d in destinations)
            GoRoute(
              path: d.path,
              pageBuilder: (context, state) => NoTransitionPage<void>(
                key: state.pageKey,
                child: d.path == '/chat'
                    ? ChatScreen(title: d.label, icon: d.icon)
                    : d.path == '/flows'
                    ? const FlowsScreen()
                    : d.path == '/runs'
                    ? const RunsScreen()
                    : d.path == '/instances'
                    ? const InstancesScreen()
                    : d.path == '/models'
                    ? const ModelsScreen()
                    : PlaceholderScreen(title: d.label, icon: d.icon),
              ),
            ),
        ],
      ),
    ],
    errorBuilder: (context, state) =>
        const Scaffold(body: Center(child: Text('Not found'))),
  );
});
