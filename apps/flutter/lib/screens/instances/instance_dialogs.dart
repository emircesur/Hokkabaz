import 'dart:io' show Platform;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'qr_scan_page.dart';
import 'instances_providers.dart';

/// Add an instance by URL + API token.
Future<void> showAddInstanceDialog(BuildContext context) =>
    showDialog<void>(context: context, builder: (_) => const _AddDialog());

/// Add an instance from a pairing link (`hokkabaz://pair?url=..&token=..`).
/// QR camera scanning lands later (P6.5); the link is what the QR carries.
Future<void> showPairDialog(BuildContext context) =>
    showDialog<void>(context: context, builder: (_) => const _PairDialog());

class _AddDialog extends ConsumerStatefulWidget {
  const _AddDialog();

  @override
  ConsumerState<_AddDialog> createState() => _AddDialogState();
}

class _AddDialogState extends ConsumerState<_AddDialog> {
  final _name = TextEditingController();
  final _url = TextEditingController();
  final _token = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _name.dispose();
    _url.dispose();
    _token.dispose();
    super.dispose();
  }

  Future<void> _submit() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await ref
        .read(instanceBookProvider.notifier)
        .add(_name.text, _url.text, _token.text);
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Add instance'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              TextField(
                controller: _name,
                decoration: const InputDecoration(
                  labelText: 'Name (optional)',
                  hintText: 'Home server',
                ),
              ),
              const SizedBox(height: HkTokens.s3),
              TextField(
                controller: _url,
                keyboardType: TextInputType.url,
                autocorrect: false,
                decoration: const InputDecoration(
                  labelText: 'URL',
                  hintText: 'https://hk.example.com',
                ),
              ),
              const SizedBox(height: HkTokens.s3),
              TextField(
                controller: _token,
                obscureText: true,
                autocorrect: false,
                enableSuggestions: false,
                decoration: const InputDecoration(labelText: 'API token'),
              ),
              if (_error != null) _ErrorText(_error!),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy ? null : _submit,
          child: const Text('Add'),
        ),
      ],
    );
  }
}

class _PairDialog extends ConsumerStatefulWidget {
  const _PairDialog();

  @override
  ConsumerState<_PairDialog> createState() => _PairDialogState();
}

class _PairDialogState extends ConsumerState<_PairDialog> {
  final _link = TextEditingController();
  final _name = TextEditingController();
  bool _busy = false;
  String? _error;

  @override
  void dispose() {
    _link.dispose();
    _name.dispose();
    super.dispose();
  }

  Future<void> _paste() async {
    final data = await Clipboard.getData(Clipboard.kTextPlain);
    if (data?.text != null) setState(() => _link.text = data!.text!.trim());
  }

  Future<void> _scan() async {
    final value = await Navigator.of(context)
        .push<String>(MaterialPageRoute(builder: (_) => const QrScanPage()));
    if (value != null && mounted) setState(() => _link.text = value.trim());
  }

  Future<void> _submit() async {
    setState(() {
      _busy = true;
      _error = null;
    });
    final err = await ref
        .read(instanceBookProvider.notifier)
        .pair(_link.text, _name.text);
    if (!mounted) return;
    if (err == null) {
      Navigator.of(context).pop();
    } else {
      setState(() {
        _busy = false;
        _error = err;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('Pair with link'),
      content: SizedBox(
        width: 420,
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(
                'Run `hokkabazd pair` on the instance and paste the link it '
                'prints. The link works once and expires after a few minutes.',
                style: Theme.of(context).textTheme.bodySmall,
              ),
              const SizedBox(height: HkTokens.s3),
              TextField(
                controller: _link,
                minLines: 2,
                maxLines: 4,
                autocorrect: false,
                enableSuggestions: false,
                decoration: InputDecoration(
                  labelText: 'Pairing link',
                  hintText: 'hokkabaz://pair?url=…&token=…',
                  suffixIcon: Row(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      if (Platform.isAndroid || Platform.isIOS)
                        IconButton(
                          tooltip: 'Scan QR code',
                          icon: const Icon(Icons.qr_code_scanner),
                          onPressed: _scan,
                        ),
                      IconButton(
                        tooltip: 'Paste',
                        icon: const Icon(Icons.content_paste),
                        onPressed: _paste,
                      ),
                    ],
                  ),
                ),
              ),
              const SizedBox(height: HkTokens.s3),
              TextField(
                controller: _name,
                decoration: const InputDecoration(labelText: 'Name (optional)'),
              ),
              if (_error != null) _ErrorText(_error!),
            ],
          ),
        ),
      ),
      actions: [
        TextButton(
          onPressed: _busy ? null : () => Navigator.of(context).pop(),
          child: const Text('Cancel'),
        ),
        FilledButton(
          onPressed: _busy ? null : _submit,
          child: const Text('Pair'),
        ),
      ],
    );
  }
}

class _ErrorText extends StatelessWidget {
  const _ErrorText(this.message);
  final String message;

  @override
  Widget build(BuildContext context) {
    return Padding(
      padding: const EdgeInsets.only(top: HkTokens.s3),
      child: Text(message, style: TextStyle(color: context.hk.err)),
    );
  }
}
