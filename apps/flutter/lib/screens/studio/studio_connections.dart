import 'package:flutter/material.dart';

import 'studio_api.dart';

Future<void> showStudioConnections(BuildContext context) async {
  final response = object(await studioRequest('connections'));
  if (!context.mounted) return;
  await showDialog<void>(
    context: context,
    builder: (_) => _ConnectionsDialog(data: response),
  );
}

class _ConnectionsDialog extends StatefulWidget {
  const _ConnectionsDialog({required this.data});
  final Json data;
  @override
  State<_ConnectionsDialog> createState() => _ConnectionsDialogState();
}

class _ConnectionsDialogState extends State<_ConnectionsDialog> {
  late final Json _config = object(widget.data['config']);
  late final Json _stt = object(_config['stt']);
  late final _model = TextEditingController(text: _config['model'] as String?);
  late final _base = TextEditingController(text: _stt['base_url'] as String?);
  late final _sttModel = TextEditingController(text: _stt['model'] as String?);
  late final _localModel = TextEditingController(
    text: _stt['model_path'] as String?,
  );
  late final _concat = TextEditingController(
    text: _config['concat_binary'] as String?,
  );
  late final _vision = TextEditingController(
    text: _config['vision_url'] as String?,
  );
  final _key = TextEditingController();
  String _keyName = 'stt';
  late String _provider = _stt['provider'] as String? ?? 'openai_compat';
  String? _error;
  bool _saving = false;

  @override
  void dispose() {
    for (final c in [
      _model,
      _base,
      _sttModel,
      _localModel,
      _concat,
      _vision,
      _key,
    ]) {
      c.dispose();
    }
    super.dispose();
  }

  Future<void> _save() async {
    setState(() {
      _saving = true;
      _error = null;
    });
    try {
      _config['model'] = _model.text.trim();
      _config['concat_binary'] = _concat.text.trim().isEmpty
          ? null
          : _concat.text.trim();
      _config['vision_url'] = _vision.text.trim().isEmpty
          ? null
          : _vision.text.trim();
      _stt['provider'] = _provider;
      _stt['base_url'] = _base.text.trim();
      _stt['model'] = _sttModel.text.trim();
      _stt['model_path'] = _localModel.text.trim().isEmpty
          ? null
          : _localModel.text.trim();
      _config['stt'] = _stt;
      await studioRequest('save_connections', _config);
      if (_key.text.isNotEmpty) {
        await studioRequest('set_secret', {
          'name': _keyName,
          'value': _key.text,
        });
        _key.clear();
      }
      if (mounted) Navigator.pop(context);
    } catch (error) {
      if (mounted) setState(() => _error = error.toString());
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  Widget _field(
    String label,
    TextEditingController controller, {
    String? helper,
  }) => Padding(
    padding: const EdgeInsets.only(bottom: 16),
    child: TextField(
      controller: controller,
      decoration: InputDecoration(
        labelText: label,
        helperText: helper,
        helperMaxLines: 3,
      ),
    ),
  );

  @override
  Widget build(BuildContext context) => AlertDialog(
    title: const Text('Studio connections'),
    content: SizedBox(
      width: 560,
      child: SingleChildScrollView(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.stretch,
          children: [
            const Text(
              'Use providers from Models, your own API endpoints, or services on your local network.',
            ),
            const SizedBox(height: 20),
            _field(
              'Analysis model · provider/model',
              _model,
              helper: 'Example: ollama/qwen3 or openai/gpt-4.1. Configure its provider in Models.',
            ),
            DropdownButtonFormField<String>(
              initialValue: _provider,
              decoration: const InputDecoration(labelText: 'Speech engine'),
              items: [
                const DropdownMenuItem(
                  value: 'openai_compat',
                  child: Text('Whisper / OpenAI-compatible API'),
                ),
                if (widget.data['local_whisper'] == true)
                  const DropdownMenuItem(
                    value: 'whisper_local',
                    child: Text('Local whisper.cpp model'),
                  ),
              ],
              onChanged: _saving
                  ? null
                  : (value) => setState(() => _provider = value!),
            ),
            const SizedBox(height: 16),
            _field(
              'Speech API URL',
              _base,
              helper: 'Example: http://192.168.1.10:8080/v1. Audio is sent to this endpoint when you transcribe.',
            ),
            _field('Speech model', _sttModel),
            if (_provider == 'whisper_local')
              _field('Local ggml model path', _localModel),
            _field(
              'Concat executable path (optional)',
              _concat,
              helper: 'Leave empty to find Concat on PATH or via HK_CONCAT.',
            ),
            _field(
              'Subject detector URL (optional)',
              _vision,
              helper: 'Your image detection endpoint. Without it, Studio uses center crop.',
            ),
            const Divider(),
            DropdownButtonFormField<String>(
              initialValue: _keyName,
              decoration: const InputDecoration(labelText: 'Connection key'),
              items: ['stt', 'pexels', 'pixabay', 'vision', 'youtube']
                  .map(
                    (name) => DropdownMenuItem(
                      value: name,
                      child: Text(
                        '$name${object(widget.data['keys'])[name] == true ? ' · saved' : ''}',
                      ),
                    ),
                  )
                  .toList(),
              onChanged: _saving
                  ? null
                  : (value) => setState(() => _keyName = value!),
            ),
            const SizedBox(height: 12),
            TextField(
              controller: _key,
              obscureText: true,
              enableSuggestions: false,
              autocorrect: false,
              decoration: const InputDecoration(
                labelText: 'API key or OAuth access token',
                helperText: 'Stored in the shared secret backend. Existing keys are never shown.',
              ),
            ),
            TextButton(
              onPressed: _saving
                  ? null
                  : () async {
                      try {
                        await studioRequest('set_secret', {
                          'name': _keyName,
                          'value': '',
                        });
                        if (mounted) {
                          setState(() {
                            (widget.data['keys'] as Map)[_keyName] = false;
                          });
                        }
                      } catch (e) {
                        if (mounted) setState(() => _error = e.toString());
                      }
                    },
              child: const Text('Remove selected key'),
            ),
            if (_error != null)
              Text(
                _error!,
                style: TextStyle(color: Theme.of(context).colorScheme.error),
              ),
          ],
        ),
      ),
    ),
    actions: [
      TextButton(
        onPressed: _saving ? null : () => Navigator.pop(context),
        child: const Text('Cancel'),
      ),
      FilledButton(
        onPressed: _saving ? null : _save,
        child: Text(_saving ? 'Saving…' : 'Save'),
      ),
    ],
  );
}
