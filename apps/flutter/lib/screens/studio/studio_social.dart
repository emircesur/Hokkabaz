import 'package:flutter/material.dart';

import 'studio_api.dart';

Future<Json?> editSocialMetadata(BuildContext context, Json metadata) =>
    showDialog<Json>(
      context: context,
      builder: (_) => _MetadataDialog(metadata: metadata),
    );

class _MetadataDialog extends StatefulWidget {
  const _MetadataDialog({required this.metadata});
  final Json metadata;
  @override
  State<_MetadataDialog> createState() => _MetadataDialogState();
}

class _MetadataDialogState extends State<_MetadataDialog> {
  late final _title = TextEditingController(
    text: widget.metadata['title'] as String,
  );
  late final _description = TextEditingController(
    text: widget.metadata['description'] as String,
  );
  late final _tags = TextEditingController(
    text: (widget.metadata['hashtags'] as List).join(' '),
  );
  @override
  void dispose() {
    _title.dispose();
    _description.dispose();
    _tags.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: Text('Edit ${widget.metadata['platform']} metadata'),
    content: SizedBox(
      width: 520,
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            TextField(
              controller: _title,
              maxLength: 100,
              decoration: const InputDecoration(labelText: 'Title'),
            ),
            TextField(
              controller: _description,
              minLines: 4,
              maxLines: 10,
              decoration: const InputDecoration(labelText: 'Description'),
            ),
            const SizedBox(height: 16),
            TextField(
              controller: _tags,
              decoration: const InputDecoration(
                labelText: 'Hashtags',
                helperText: 'Separate tags with spaces',
              ),
            ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: () => Navigator.pop(context, {
          ...widget.metadata,
          'title': _title.text.trim(),
          'description': _description.text.trim(),
          'hashtags': _tags.text
              .split(RegExp(r'\s+'))
              .where((s) => s.isNotEmpty)
              .toList(),
        }),
        child: const Text('Save'),
      ),
    ],
  );
}
