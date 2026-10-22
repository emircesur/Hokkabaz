import 'dart:convert';

import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'model/run_overlay.dart';

const _maxRows = 100;
const _maxCols = 12;
const _maxCell = 80;
const _maxJsonItems = 50;

/// Table columns of [items]: the union of their `json` keys in first-seen order.
List<String> itemColumns(List<FlowItem> items) {
  final cols = <String>{};
  for (final it in items) {
    final j = it['json'];
    if (j is Map) cols.addAll(j.keys.cast<String>());
    if (cols.length >= _maxCols) break;
  }
  return cols.take(_maxCols).toList();
}

/// One table cell: scalars as text, nested values as compact JSON, long text cut.
String cellText(Object? v) {
  final s = v == null
      ? ''
      : v is String
      ? v
      : v is num || v is bool
      ? '$v'
      : jsonEncode(v);
  return s.length > _maxCell ? '${s.substring(0, _maxCell)}...' : s;
}

/// Items of a node run as a table or as JSON. [total] is the true count when the
/// list was cut by the core.
class ItemsView extends StatefulWidget {
  const ItemsView({
    super.key,
    required this.items,
    this.total,
    this.empty = 'No items',
  });

  final List<FlowItem> items;
  final int? total;
  final String empty;

  @override
  State<ItemsView> createState() => _ItemsViewState();
}

class _ItemsViewState extends State<ItemsView> {
  bool _table = true;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final items = widget.items;
    if (items.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(HkTokens.s3),
        child: Text(
          widget.empty,
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
      );
    }
    final total = widget.total ?? items.length;
    final cut = total > items.length;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Padding(
          padding: const EdgeInsets.symmetric(
            horizontal: HkTokens.s3,
            vertical: HkTokens.s1,
          ),
          child: Row(
            children: [
              Expanded(
                child: Text(
                  cut
                      ? '${itemsLabel(total)} (first ${items.length} shown)'
                      : itemsLabel(total),
                  style: text.bodySmall?.copyWith(color: hk.textMuted),
                ),
              ),
              SegmentedButton<bool>(
                showSelectedIcon: false,
                style: const ButtonStyle(
                  minimumSize: WidgetStatePropertyAll(Size(48, 48)),
                  visualDensity: VisualDensity.compact,
                ),
                segments: const [
                  ButtonSegment(value: true, label: Text('Table')),
                  ButtonSegment(value: false, label: Text('JSON')),
                ],
                selected: {_table},
                onSelectionChanged: (s) => setState(() => _table = s.first),
              ),
            ],
          ),
        ),
        if (_table) _tableView(context) else _json(context),
      ],
    );
  }

  Widget _tableView(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final cols = itemColumns(widget.items);
    if (cols.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(HkTokens.s3),
        child: Text(
          'Items have no JSON fields',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
      );
    }
    final rows = widget.items.take(_maxRows).toList();
    return SingleChildScrollView(
      scrollDirection: Axis.horizontal,
      child: DataTable(
        columnSpacing: HkTokens.s4,
        headingRowHeight: 40,
        dataRowMinHeight: 36,
        dataRowMaxHeight: 48,
        columns: [
          const DataColumn(label: Text('#')),
          for (final c in cols) DataColumn(label: Text(c)),
        ],
        rows: [
          for (var i = 0; i < rows.length; i++)
            DataRow(
              cells: [
                DataCell(Text('$i', style: TextStyle(color: hk.textMuted))),
                for (final c in cols)
                  DataCell(
                    Text(
                      cellText((rows[i]['json'] as Map?)?[c]),
                      style: text.bodySmall,
                    ),
                  ),
              ],
            ),
        ],
      ),
    );
  }

  Widget _json(BuildContext context) {
    final hk = context.hk;
    final shown = widget.items.take(_maxJsonItems).toList();
    return Container(
      margin: const EdgeInsets.all(HkTokens.s3),
      padding: const EdgeInsets.all(HkTokens.s2),
      decoration: BoxDecoration(
        color: hk.surface,
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
        border: Border.all(color: hk.border),
      ),
      child: SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: SelectableText(
          const JsonEncoder.withIndent('  ').convert(shown),
          style: const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 12),
        ),
      ),
    );
  }
}
