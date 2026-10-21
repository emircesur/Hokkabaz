import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'model/node_types.dart';
import 'node_card.dart';

/// Result of the picker: a catalog type, or a raw `type` string typed by the
/// user (becomes an unsupported placeholder when the registry lacks it).
class NodePick {
  const NodePick.type(NodeTypeInfo this.info) : customType = null;
  const NodePick.custom(String this.customType) : info = null;

  final NodeTypeInfo? info;
  final String? customType;
}

/// Search across display name, type, description, group and category.
List<NodeTypeInfo> searchNodeTypes(List<NodeTypeInfo> all, String query) {
  final q = query.trim().toLowerCase();
  if (q.isEmpty) return all;
  final terms = q.split(RegExp(r'\s+'));
  int score(NodeTypeInfo t) {
    final name = t.displayName.toLowerCase();
    final hay = [
      name,
      t.typeName.toLowerCase(),
      t.description.toLowerCase(),
      t.group.join(' ').toLowerCase(),
      t.category.label.toLowerCase(),
    ].join(' ');
    if (!terms.every(hay.contains)) return -1;
    if (name == q) return 3;
    if (name.startsWith(q)) return 2;
    if (name.contains(q)) return 1;
    return 0;
  }

  final scored = <(NodeTypeInfo, int)>[
    for (final t in all)
      if (score(t) >= 0) (t, score(t)),
  ];
  scored.sort((a, b) => b.$2.compareTo(a.$2));
  return [for (final s in scored) s.$1];
}

Future<NodePick?> showNodePicker(
  BuildContext context,
  List<NodeTypeInfo> types,
) {
  final wide = MediaQuery.sizeOf(context).width >= 640;
  final body = _PickerBody(types: types);
  if (wide) {
    return showDialog<NodePick>(
      context: context,
      builder: (_) => Dialog(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 520, maxHeight: 560),
          child: body,
        ),
      ),
    );
  }
  return showModalBottomSheet<NodePick>(
    context: context,
    isScrollControlled: true,
    useSafeArea: true,
    builder: (_) => Padding(
      padding: EdgeInsets.only(bottom: MediaQuery.viewInsetsOf(context).bottom),
      child: SizedBox(
        height: MediaQuery.sizeOf(context).height * 0.85,
        child: body,
      ),
    ),
  );
}

class _PickerBody extends StatefulWidget {
  const _PickerBody({required this.types});
  final List<NodeTypeInfo> types;

  @override
  State<_PickerBody> createState() => _PickerBodyState();
}

class _PickerBodyState extends State<_PickerBody> {
  String _q = '';

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final results = searchNodeTypes(widget.types, _q);
    final qt = _q.trim();
    // A dotted string looks like a node type: offer it as an unsupported placeholder.
    final custom = qt.contains('.') && !qt.contains(' ') ? qt : null;

    // Group by category when browsing; flat ranked list when searching.
    final children = <Widget>[];
    if (qt.isEmpty) {
      for (final c in NodeCategory.values) {
        final items = results.where((t) => t.category == c).toList();
        if (items.isEmpty) continue;
        children.add(
          Padding(
            padding: const EdgeInsets.fromLTRB(
              HkTokens.s4,
              HkTokens.s3,
              HkTokens.s4,
              HkTokens.s1,
            ),
            child: Text(
              c.label.toUpperCase(),
              style: text.labelSmall?.copyWith(color: hk.textMuted),
            ),
          ),
        );
        children.addAll(items.map(_tile));
      }
    } else {
      children.addAll(results.map(_tile));
    }
    if (custom != null) {
      children.add(
        ListTile(
          dense: true,
          leading: Icon(Icons.warning_amber_rounded, color: hk.warn),
          title: Text('Add "$custom" as placeholder'),
          subtitle: const Text(
            'Type is not in this build; the node is kept as JSON.',
          ),
          onTap: () => Navigator.pop(context, NodePick.custom(custom)),
        ),
      );
    }
    if (children.isEmpty) {
      children.add(
        Padding(
          padding: const EdgeInsets.all(HkTokens.s6),
          child: Center(
            child: Text(
              widget.types.isEmpty
                  ? 'No node types loaded'
                  : 'No matching nodes',
              style: text.bodySmall,
            ),
          ),
        ),
      );
    }

    return Column(
      children: [
        Padding(
          padding: const EdgeInsets.all(HkTokens.s3),
          child: TextField(
            autofocus: true,
            decoration: const InputDecoration(
              prefixIcon: Icon(Icons.search),
              hintText: 'Search nodes: name, category, or type',
              isDense: true,
            ),
            onChanged: (v) => setState(() => _q = v),
            onSubmitted: (_) {
              if (results.isNotEmpty) {
                Navigator.pop(context, NodePick.type(results.first));
              } else if (custom != null) {
                Navigator.pop(context, NodePick.custom(custom));
              }
            },
          ),
        ),
        Divider(height: 1, color: hk.border),
        Expanded(child: ListView(children: children)),
      ],
    );
  }

  Widget _tile(NodeTypeInfo t) {
    final c = categoryColor(t.category);
    return ListTile(
      dense: true,
      leading: Icon(categoryIcon(t.category), color: c),
      title: Text(t.displayName),
      subtitle: t.description.isEmpty
          ? null
          : Text(t.description, maxLines: 1, overflow: TextOverflow.ellipsis),
      trailing: Text(
        shortType(t.typeName),
        style: Theme.of(context).textTheme.bodySmall
            ?.copyWith(color: context.hk.textMuted),
      ),
      onTap: () => Navigator.pop(context, NodePick.type(t)),
    );
  }
}
