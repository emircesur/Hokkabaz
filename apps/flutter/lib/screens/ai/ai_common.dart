import 'dart:convert';

import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;

/// Run `fn`, report errors (and an optional success line) in a SnackBar.
Future<T?> guarded<T>(
  BuildContext context,
  Future<T> Function() fn, {
  String? ok,
}) async {
  try {
    final v = await fn();
    if (ok != null && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(ok)));
    }
    return v;
  } catch (e) {
    if (context.mounted) {
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(describeError(e))));
    }
    return null;
  }
}

String prettyJson(Object? v) => const JsonEncoder.withIndent('  ').convert(v);

/// Page frame for one AI hub section.
class SectionFrame extends StatelessWidget {
  const SectionFrame({
    super.key,
    required this.title,
    this.subtitle,
    this.actions = const [],
    required this.child,
  });

  final String title;
  final String? subtitle;
  final List<Widget> actions;
  final Widget child;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Padding(
      padding: const EdgeInsets.all(HkTokens.s4),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  title,
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
              ),
              ...actions,
            ],
          ),
          if (subtitle != null) ...[
            const SizedBox(height: HkTokens.s1),
            Text(subtitle!, style: TextStyle(color: hk.textMuted)),
          ],
          const SizedBox(height: HkTokens.s3),
          Expanded(child: child),
        ],
      ),
    );
  }
}

/// Field spec for [editItem].
class FieldSpec {
  const FieldSpec(
    this.key,
    this.label, {
    this.multiline = false,
    this.number = false,
    this.hint,
    this.list = false,
  });
  final String key;
  final String label;
  final bool multiline;
  final bool number;
  final bool list;
  final String? hint;
}

/// Simple form dialog over a JSON map; returns the edited copy or null.
Future<Map<String, dynamic>?> editItem(
  BuildContext context, {
  required String title,
  required List<FieldSpec> fields,
  Map<String, dynamic> initial = const {},
}) {
  final ctls = {
    for (final f in fields)
      f.key: TextEditingController(
        text: switch (initial[f.key]) {
          null => '',
          final List<dynamic> l => l.join(', '),
          final Object v => v.toString(),
        },
      ),
  };
  return showDialog<Map<String, dynamic>>(
    context: context,
    builder: (ctx) => AlertDialog(
      title: Text(title),
      content: SizedBox(
        width: 520,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              for (final f in fields)
                Padding(
                  padding: const EdgeInsets.only(bottom: HkTokens.s2),
                  child: TextField(
                    controller: ctls[f.key],
                    minLines: f.multiline ? 3 : 1,
                    maxLines: f.multiline ? 10 : 1,
                    keyboardType: f.number ? TextInputType.number : null,
                    decoration: InputDecoration(
                      labelText: f.label,
                      hintText: f.hint,
                      border: const OutlineInputBorder(),
                    ),
                  ),
                ),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.pop(ctx),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: () {
            final out = Map<String, dynamic>.of(initial);
            for (final f in fields) {
              final t = ctls[f.key]!.text.trim();
              if (f.list) {
                out[f.key] = [
                  for (final x in t.split(','))
                    if (x.trim().isNotEmpty) x.trim(),
                ];
              } else if (f.number) {
                out[f.key] = num.tryParse(t);
              } else {
                out[f.key] = t;
              }
            }
            Navigator.pop(ctx, out);
          },
          child: const Text('Save'),
        ),
      ],
    ),
  );
}

/// Card row used by list sections.
class ItemTile extends StatelessWidget {
  const ItemTile({
    super.key,
    required this.title,
    this.subtitle,
    this.leading,
    this.onTap,
    this.trailing = const [],
  });

  final String title;
  final String? subtitle;
  final IconData? leading;
  final VoidCallback? onTap;
  final List<Widget> trailing;

  @override
  Widget build(BuildContext context) => Card(
    margin: const EdgeInsets.only(bottom: HkTokens.s2),
    child: ListTile(
      leading: leading == null ? null : Icon(leading),
      title: Text(title, maxLines: 1, overflow: TextOverflow.ellipsis),
      subtitle: subtitle == null
          ? null
          : Text(subtitle!, maxLines: 2, overflow: TextOverflow.ellipsis),
      onTap: onTap,
      trailing: trailing.isEmpty
          ? null
          : Row(mainAxisSize: MainAxisSize.min, children: trailing),
    ),
  );
}

String newId(String prefix) =>
    '${prefix}_${DateTime.now().microsecondsSinceEpoch.toRadixString(36)}';
