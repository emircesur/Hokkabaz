import 'dart:convert';

import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'model/node_types.dart';
import 'model/workflow_model.dart';

/// Parameter form generated from n8n node description properties (DESIGN §4.4).
///
/// Edits only the keys it touches, so parameters the description does not
/// know (or widgets we render as raw JSON) survive untouched. [onChanged]
/// gets the full new parameters map plus a coalescing key so a typing burst
/// becomes one undo step.
class ParamsForm extends StatelessWidget {
  const ParamsForm({
    super.key,
    required this.specs,
    required this.values,
    required this.onChanged,
    this.typeVersion = 1,
  });

  final List<ParamSpec> specs;
  final Map<String, dynamic> values;
  final num typeVersion;
  final void Function(Map<String, dynamic> next, String coalesceKey) onChanged;

  @override
  Widget build(BuildContext context) {
    final visible = [
      for (final s in specs)
        if (s.type != 'hidden' && s.isVisible(values, typeVersion: typeVersion)) s,
    ];
    if (visible.isEmpty) {
      return Padding(
        padding: const EdgeInsets.all(HkTokens.s3),
        child: Text('No parameters', style: Theme.of(context).textTheme.bodySmall),
      );
    }
    return _FieldList(
      specs: visible,
      values: values,
      path: '',
      typeVersion: typeVersion,
      onChanged: onChanged,
    );
  }
}

/// Shared by the root form and nested collections.
class _FieldList extends StatelessWidget {
  const _FieldList({
    required this.specs,
    required this.values,
    required this.path,
    required this.typeVersion,
    required this.onChanged,
    this.removable = false,
  });

  final List<ParamSpec> specs;
  final Map<String, dynamic> values;
  final String path;
  final num typeVersion;
  final bool removable;
  final void Function(Map<String, dynamic> next, String key) onChanged;

  void _set(ParamSpec s, Object? v) {
    final next = Map<String, dynamic>.of(values);
    if (v == null) {
      next.remove(s.name);
    } else {
      next[s.name] = v;
    }
    onChanged(next, '$path/${s.name}');
  }

  @override
  Widget build(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        for (final s in specs)
          Padding(
            padding: const EdgeInsets.only(bottom: HkTokens.s3),
            child: ParamField(
              key: ValueKey('$path/${s.name}'),
              spec: s,
              value: values.containsKey(s.name) ? values[s.name] : null,
              path: '$path/${s.name}',
              typeVersion: typeVersion,
              onChanged: (v) => _set(s, v),
              onRemove: removable ? () => _set(s, null) : null,
            ),
          ),
      ],
    );
  }
}

bool _isExpr(Object? v) => v is String && v.startsWith('=');

class ParamField extends StatelessWidget {
  const ParamField({
    super.key,
    required this.spec,
    required this.value,
    required this.path,
    required this.typeVersion,
    required this.onChanged,
    this.onRemove,
  });

  final ParamSpec spec;
  final Object? value;
  final String path;
  final num typeVersion;
  final void Function(Object? v) onChanged;
  final VoidCallback? onRemove;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final label = spec.required ? '${spec.displayName} *' : spec.displayName;
    final help = spec.description ?? spec.hint;
    final shown = value ?? spec.defaultValue;

    Widget field;
    switch (spec.type) {
      case 'notice':
        return Container(
          padding: const EdgeInsets.all(HkTokens.s3),
          decoration: BoxDecoration(
            color: hk.info.withValues(alpha: 0.1),
            borderRadius: BorderRadius.circular(HkTokens.radiusCard),
          ),
          child: Text(spec.displayName, style: text.bodySmall),
        );
      case 'boolean' when !_isExpr(shown):
        field = SwitchListTile(
          contentPadding: EdgeInsets.zero,
          dense: true,
          title: Text(label),
          subtitle: help == null ? null : Text(help, style: text.bodySmall),
          value: shown == true,
          onChanged: onChanged,
        );
        return _withRemove(field);
      case 'options' when !_isExpr(shown):
        field = _OptionsField(spec: spec, label: label, help: help, value: shown, onChanged: onChanged);
      case 'multiOptions':
        field = _MultiOptionsField(spec: spec, label: label, value: shown, onChanged: onChanged);
      case 'collection':
        field = _CollectionField(
          spec: spec,
          label: label,
          value: value is Map<String, dynamic> ? value! as Map<String, dynamic> : const {},
          path: path,
          typeVersion: typeVersion,
          onChanged: onChanged,
        );
      case 'fixedCollection':
        field = _FixedCollectionField(
          spec: spec,
          label: label,
          value: value is Map<String, dynamic> ? value! as Map<String, dynamic> : const {},
          path: path,
          typeVersion: typeVersion,
          onChanged: onChanged,
        );
      case 'string' || 'number' || 'dateTime' || 'color' || 'json' || 'options' || 'boolean':
        field = _ScalarField(spec: spec, label: label, help: help, value: shown, onChanged: onChanged);
      default:
        // filter, assignmentCollection, workflowSelector, future widgets: edit the raw JSON.
        field = _JsonField(label: label, help: help, value: value ?? spec.defaultValue, onChanged: onChanged);
    }
    return _withRemove(field);
  }

  Widget _withRemove(Widget child) {
    if (onRemove == null) return child;
    return Row(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Expanded(child: child),
        IconButton(
          tooltip: 'Remove',
          visualDensity: VisualDensity.compact,
          icon: const Icon(Icons.close, size: 16),
          onPressed: onRemove,
        ),
      ],
    );
  }
}

/// string / number / dateTime / color / json, plus any value that is an `={{ }}` expression.
class _ScalarField extends StatefulWidget {
  const _ScalarField({
    required this.spec,
    required this.label,
    required this.help,
    required this.value,
    required this.onChanged,
  });

  final ParamSpec spec;
  final String label;
  final String? help;
  final Object? value;
  final void Function(Object?) onChanged;

  @override
  State<_ScalarField> createState() => _ScalarFieldState();
}

class _ScalarFieldState extends State<_ScalarField> {
  late final TextEditingController _c = TextEditingController(text: _show(widget.value));
  final _focus = FocusNode();

  static String _show(Object? v) {
    if (v == null) return '';
    if (v is String) return v;
    return jsonEncode(v);
  }

  @override
  void didUpdateWidget(_ScalarField old) {
    super.didUpdateWidget(old);
    // Undo/redo or a sibling edit changed the value while this field is idle.
    final t = _show(widget.value);
    if (!_focus.hasFocus && _c.text != t) _c.text = t;
  }

  @override
  void dispose() {
    _c.dispose();
    _focus.dispose();
    super.dispose();
  }

  bool get _number => widget.spec.type == 'number';
  bool get _expr => _isExpr(widget.value);

  void _emit(String t) {
    if (_number && !t.startsWith('=')) {
      if (t.trim().isEmpty) return widget.onChanged(null);
      final n = int.tryParse(t.trim()) ?? double.tryParse(t.trim());
      if (n == null) return; // keep typing; not a number yet
      return widget.onChanged(n);
    }
    widget.onChanged(t);
  }

  void _toggleExpr() {
    final t = _c.text;
    final next = t.startsWith('=') ? t.substring(1) : '=$t';
    _c.text = next;
    _emit(next);
  }

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final mono = widget.spec.type == 'json' || _expr;
    final rows = widget.spec.type == 'json' ? 6 : widget.spec.rows;
    final canExpr = widget.spec.type != 'json' && widget.spec.typeOptions['password'] != true;
    return TextField(
      controller: _c,
      focusNode: _focus,
      minLines: rows > 1 ? rows : 1,
      maxLines: rows > 1 ? rows : 1,
      obscureText: widget.spec.typeOptions['password'] == true,
      keyboardType: _number ? const TextInputType.numberWithOptions(decimal: true, signed: true) : null,
      style: mono ? const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 13) : null,
      decoration: InputDecoration(
        labelText: widget.label,
        helperText: widget.help,
        helperMaxLines: 3,
        hintText: widget.spec.placeholder,
        isDense: true,
        suffixIcon: canExpr
            ? IconButton(
                tooltip: _expr ? 'Fixed value' : 'Use expression',
                visualDensity: VisualDensity.compact,
                icon: Text(
                  'fx',
                  style: TextStyle(
                    fontWeight: FontWeight.w700,
                    color: _expr ? hk.accent : hk.textMuted,
                  ),
                ),
                onPressed: _toggleExpr,
              )
            : null,
      ),
      onChanged: _emit,
    );
  }
}

class _OptionsField extends StatelessWidget {
  const _OptionsField({
    required this.spec,
    required this.label,
    required this.help,
    required this.value,
    required this.onChanged,
  });

  final ParamSpec spec;
  final String label;
  final String? help;
  final Object? value;
  final void Function(Object?) onChanged;

  @override
  Widget build(BuildContext context) {
    final known = spec.options.any((o) => o.value == value);
    return DropdownButtonFormField<Object?>(
      initialValue: value,
      isExpanded: true,
      decoration: InputDecoration(labelText: label, helperText: help, helperMaxLines: 3, isDense: true),
      items: [
        for (final o in spec.options) DropdownMenuItem(value: o.value, child: Text(o.name, overflow: TextOverflow.ellipsis)),
        if (!known && value != null) DropdownMenuItem(value: value, child: Text('$value')),
      ],
      onChanged: onChanged,
    );
  }
}

class _MultiOptionsField extends StatelessWidget {
  const _MultiOptionsField({
    required this.spec,
    required this.label,
    required this.value,
    required this.onChanged,
  });

  final ParamSpec spec;
  final String label;
  final Object? value;
  final void Function(Object?) onChanged;

  @override
  Widget build(BuildContext context) {
    final cur = value is List ? List<Object?>.of(value! as List) : <Object?>[];
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(label, style: Theme.of(context).textTheme.labelMedium),
        const SizedBox(height: HkTokens.s1),
        Wrap(
          spacing: HkTokens.s1,
          children: [
            for (final o in spec.options)
              FilterChip(
                label: Text(o.name),
                selected: cur.contains(o.value),
                onSelected: (on) {
                  final next = List<Object?>.of(cur);
                  on ? next.add(o.value) : next.remove(o.value);
                  onChanged(next);
                },
              ),
          ],
        ),
      ],
    );
  }
}

Map<String, dynamic> _defaultsFor(List<ParamSpec> specs) => {
  for (final s in specs)
    if (s.defaultValue != null &&
        s.type != 'notice' &&
        s.type != 'collection' &&
        s.type != 'fixedCollection')
      s.name: deepCopy(s.defaultValue),
};

class _Section extends StatelessWidget {
  const _Section({required this.title, required this.child, this.trailing});
  final String title;
  final Widget child;
  final Widget? trailing;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    return Container(
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        border: Border.all(color: hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          Row(
            children: [
              Expanded(child: Text(title, style: Theme.of(context).textTheme.labelLarge)),
              ?trailing,
            ],
          ),
          const SizedBox(height: HkTokens.s2),
          child,
        ],
      ),
    );
  }
}

/// n8n `collection`: optional sub-fields, added one by one from a menu.
class _CollectionField extends StatelessWidget {
  const _CollectionField({
    required this.spec,
    required this.label,
    required this.value,
    required this.path,
    required this.typeVersion,
    required this.onChanged,
  });

  final ParamSpec spec;
  final String label;
  final Map<String, dynamic> value;
  final String path;
  final num typeVersion;
  final void Function(Object?) onChanged;

  @override
  Widget build(BuildContext context) {
    final present = [
      for (final c in spec.children)
        if (value.containsKey(c.name) && c.isVisible(value, typeVersion: typeVersion)) c,
    ];
    final addable = [
      for (final c in spec.children)
        if (!value.containsKey(c.name)) c,
    ];
    return _Section(
      title: label,
      trailing: addable.isEmpty
          ? null
          : PopupMenuButton<ParamSpec>(
              tooltip: spec.placeholder ?? 'Add option',
              icon: const Icon(Icons.add, size: 18),
              onSelected: (c) => onChanged({
                ...value,
                c.name: deepCopy(c.defaultValue ?? _emptyFor(c)),
              }),
              itemBuilder: (_) => [
                for (final c in addable) PopupMenuItem(value: c, child: Text(c.displayName)),
              ],
            ),
      child: present.isEmpty
          ? Text(spec.placeholder ?? 'No options set', style: Theme.of(context).textTheme.bodySmall)
          : _FieldList(
              specs: present,
              values: value,
              path: path,
              typeVersion: typeVersion,
              removable: true,
              onChanged: (next, _) => onChanged(next),
            ),
    );
  }

  static Object? _emptyFor(ParamSpec c) => switch (c.type) {
    'boolean' => false,
    'number' => 0,
    'collection' || 'fixedCollection' => <String, dynamic>{},
    'multiOptions' => <Object?>[],
    _ => '',
  };
}

/// n8n `fixedCollection`: named groups, each one item or a list of items.
class _FixedCollectionField extends StatelessWidget {
  const _FixedCollectionField({
    required this.spec,
    required this.label,
    required this.value,
    required this.path,
    required this.typeVersion,
    required this.onChanged,
  });

  final ParamSpec spec;
  final String label;
  final Map<String, dynamic> value;
  final String path;
  final num typeVersion;
  final void Function(Object?) onChanged;

  @override
  Widget build(BuildContext context) {
    final multi = spec.multipleValues;
    return _Section(
      title: label,
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.stretch,
        children: [
          for (final g in spec.groups) ...[
            if (spec.groups.length > 1)
              Padding(
                padding: const EdgeInsets.only(bottom: HkTokens.s1),
                child: Text(g.displayName, style: Theme.of(context).textTheme.labelMedium),
              ),
            if (multi)
              ..._rows(context, g)
            else
              _single(context, g),
          ],
        ],
      ),
    );
  }

  List<Widget> _rows(BuildContext context, ParamGroup g) {
    final raw = value[g.name];
    final items = raw is List
        ? [for (final i in raw) i is Map<String, dynamic> ? i : <String, dynamic>{}]
        : <Map<String, dynamic>>[];
    void emit(List<Map<String, dynamic>> next) {
      final out = Map<String, dynamic>.of(value);
      if (next.isEmpty) {
        out.remove(g.name);
      } else {
        out[g.name] = next;
      }
      onChanged(out);
    }

    return [
      for (var i = 0; i < items.length; i++)
        Padding(
          padding: const EdgeInsets.only(bottom: HkTokens.s2),
          child: _Section(
            title: '${g.displayName} ${i + 1}',
            trailing: IconButton(
              tooltip: 'Delete',
              visualDensity: VisualDensity.compact,
              icon: const Icon(Icons.delete_outline, size: 18),
              onPressed: () => emit([...items]..removeAt(i)),
            ),
            child: _FieldList(
              specs: [
                for (final s in g.values)
                  if (s.type != 'hidden' && s.isVisible(items[i], typeVersion: typeVersion)) s,
              ],
              values: items[i],
              path: '$path/${g.name}/$i',
              typeVersion: typeVersion,
              onChanged: (next, _) {
                final copy = [...items];
                copy[i] = next;
                emit(copy);
              },
            ),
          ),
        ),
      Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          icon: const Icon(Icons.add, size: 16),
          label: Text('Add ${g.displayName}'),
          onPressed: () => emit([...items, _defaultsFor(g.values)]),
        ),
      ),
    ];
  }

  Widget _single(BuildContext context, ParamGroup g) {
    final raw = value[g.name];
    if (raw is! Map<String, dynamic>) {
      return Align(
        alignment: Alignment.centerLeft,
        child: TextButton.icon(
          icon: const Icon(Icons.add, size: 16),
          label: Text('Add ${g.displayName}'),
          onPressed: () => onChanged({...value, g.name: _defaultsFor(g.values)}),
        ),
      );
    }
    return _FieldList(
      specs: [
        for (final s in g.values)
          if (s.type != 'hidden' && s.isVisible(raw, typeVersion: typeVersion)) s,
      ],
      values: raw,
      path: '$path/${g.name}',
      typeVersion: typeVersion,
      onChanged: (next, _) => onChanged({...value, g.name: next}),
    );
  }
}

/// Raw JSON editor for widgets without a dedicated form (filter conditions,
/// assignments, ...) and for parameters of unsupported nodes. Applies only
/// when the text parses, so a half-typed edit never corrupts the node.
class JsonValueField extends StatelessWidget {
  const JsonValueField({
    super.key,
    required this.label,
    required this.value,
    required this.onChanged,
    this.help,
  });

  final String label;
  final String? help;
  final Object? value;
  final void Function(Object?) onChanged;

  @override
  Widget build(BuildContext context) =>
      _JsonField(label: label, help: help, value: value, onChanged: onChanged);
}

class _JsonField extends StatefulWidget {
  const _JsonField({
    required this.label,
    required this.help,
    required this.value,
    required this.onChanged,
  });

  final String label;
  final String? help;
  final Object? value;
  final void Function(Object?) onChanged;

  @override
  State<_JsonField> createState() => _JsonFieldState();
}

class _JsonFieldState extends State<_JsonField> {
  static const _enc = JsonEncoder.withIndent('  ');
  late final TextEditingController _c = TextEditingController(text: _fmt(widget.value));
  final _focus = FocusNode();
  String? _error;

  static String _fmt(Object? v) => v == null ? '' : _enc.convert(v);

  @override
  void didUpdateWidget(_JsonField old) {
    super.didUpdateWidget(old);
    if (!_focus.hasFocus && _c.text != _fmt(widget.value)) {
      _c.text = _fmt(widget.value);
    }
  }

  @override
  void dispose() {
    _c.dispose();
    _focus.dispose();
    super.dispose();
  }

  void _edit(String t) {
    if (t.trim().isEmpty) {
      setState(() => _error = null);
      return widget.onChanged(null);
    }
    try {
      final v = jsonDecode(t);
      setState(() => _error = null);
      widget.onChanged(v);
    } on FormatException catch (e) {
      setState(() => _error = 'Invalid JSON: ${e.message}');
    }
  }

  @override
  Widget build(BuildContext context) {
    return TextField(
      controller: _c,
      focusNode: _focus,
      minLines: 4,
      maxLines: 12,
      style: const TextStyle(fontFamily: HkTokens.fontMono, fontSize: 13),
      decoration: InputDecoration(
        labelText: '${widget.label} (JSON)',
        helperText: widget.help,
        helperMaxLines: 3,
        errorText: _error,
        errorMaxLines: 2,
        isDense: true,
        alignLabelWithHint: true,
      ),
      onChanged: _edit,
    );
  }
}
