import 'dart:async';
import 'dart:convert';

import 'package:file_picker/file_picker.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../ffi/ffi.dart' show rustInitErrorProvider;
import '../../src/rust/api/local_models.dart' as api;
import '../../ffi/hk_gate.dart' as gate;
import '../../theme/tokens.dart';
import '../chat/chat_providers.dart' show describeError;

class LocalModel {
  LocalModel(Map<String, dynamic> j)
    : id = j['id'] as String,
      name = j['name'] as String,
      note = j['note'] as String? ?? '',
      sizeBytes = (j['size_bytes'] as num).toInt(),
      ramNeeded = (j['ram_needed'] as num?)?.toInt() ?? 0,
      fit = j['fit'] as String? ?? 'good',
      installed = j['installed'] == true,
      downloading = j['downloading'] == true,
      license = j['license'] as String? ?? '';
  final String id, name, note, fit, license;
  final int sizeBytes, ramNeeded;
  final bool installed, downloading;
}

class LocalOverview {
  LocalOverview(Map<String, dynamic> j)
    : engine = j['engine'] == true,
      totalRam = ((j['device'] as Map)['total_ram'] as num).toInt(),
      recommended = j['recommended'] as String?,
      fallback = j['fallback'] as String?,
      models = (j['models'] as List).map((m) => LocalModel((m as Map).cast())).toList(),
      custom = (j['custom'] as List).map((m) => (m as Map).cast<String, dynamic>()).toList(),
      gpuBackend = (j['gpu'] as Map?)?['backend'] as String?,
      gpu = (((j['gpu'] as Map?)?['settings'] as Map?) ?? const {'enabled': true, 'device': 0}).cast<String, dynamic>(),
      gpuDevices = [for (final d in ((j['gpu'] as Map?)?['devices'] as List? ?? const [])) (d as Map).cast<String, dynamic>()],
      loaded = [for (final m in (j['loaded'] as List? ?? const [])) '$m'];
  final bool engine;

  /// Models currently in memory.
  final List<String> loaded;

  /// `vulkan` | `cuda` | `metal`, or null when local models run on the CPU only.
  final String? gpuBackend;

  /// `{enabled, device, layers}` (layers null = all).
  final Map<String, dynamic> gpu;

  /// `[{position, description, memory_total, integrated}]` from the llama.cpp backend.
  final List<Map<String, dynamic>> gpuDevices;
  final int totalRam;
  final String? recommended, fallback;
  final List<LocalModel> models;
  final List<Map<String, dynamic>> custom;

  /// Everything installed, for the fallback picker.
  List<(String, String)> get installedChoices => [
    for (final m in models.where((m) => m.installed)) (m.id, m.name),
    for (final c in custom) ('${c['id']}', '${c['name']}'),
  ];
}

class LocalModelsNotifier extends AsyncNotifier<LocalOverview> {
  final Map<String, StreamSubscription<String>> _subs = {};
  final Map<String, double> progress = {};

  @override
  Future<LocalOverview> build() async {
    if (ref.watch(rustInitErrorProvider) != null) throw StateError('Rust is not available');
    ref.onDispose(() {
      for (final s in _subs.values) {
        s.cancel();
      }
    });
    return _load();
  }

  Future<LocalOverview> _load() async => LocalOverview(jsonDecode(await api.localModelsOverview()) as Map<String, dynamic>);

  Future<void> refresh() async => state = await AsyncValue.guard(_load);

  Future<void> reload() async => state = AsyncData(await _load());

  /// Starts a download; progress (0..1) is in [progress] and the state is re-emitted.
  Future<String?> download(String id, int total) {
    final done = Completer<String?>();
    progress[id] = 0;
    late StreamSubscription<String> sub;
    sub = api.localModelDownload(id: id).listen(
      (ev) {
        final j = jsonDecode(ev) as Map;
        progress[id] = ((j['done'] as num) / (j['total'] as num).clamp(1, double.infinity)).toDouble();
        ref.notifyListeners();
      },
      onError: (Object e) {
        progress.remove(id);
        _subs.remove(id);
        done.complete(describeError(e));
      },
      onDone: () async {
        progress.remove(id);
        _subs.remove(id);
        await reload();
        if (!done.isCompleted) done.complete(null);
      },
      cancelOnError: true,
    );
    _subs[id] = sub;
    return done.future;
  }

  Future<void> cancel(String id) => api.localModelCancel(id: id);

  Future<String?> delete(String id) async {
    try {
      await api.localModelDelete(id: id);
      await reload();
      return null;
    } catch (e) {
      return describeError(e);
    }
  }

  Future<String?> addFile(String path) async {
    try {
      await api.localModelAddFile(path: path);
      await reload();
      return null;
    } catch (e) {
      return describeError(e);
    }
  }

  Future<String?> setGpu({required bool enabled, required int device, int? layers}) async {
    try {
      await api.localSetGpu(settingsJson: jsonEncode({'enabled': enabled, 'device': device, 'layers': layers}));
      await reload();
      return null;
    } catch (e) {
      return describeError(e);
    }
  }

  Future<String?> setFallback(String? id) async {
    try {
      await api.localSetFallback(id: id);
      await reload();
      return null;
    } catch (e) {
      return describeError(e);
    }
  }
}

final localModelsProvider = AsyncNotifierProvider<LocalModelsNotifier, LocalOverview>(LocalModelsNotifier.new);

String formatBytes(int b) {
  const gb = 1024 * 1024 * 1024;
  if (b >= gb) return '${(b / gb).toStringAsFixed(1)} GB';
  return '${(b / (1024 * 1024)).round()} MB';
}

/// DESIGN P6.8: on-device models. Browse, download (resumable), delete, pick the offline fallback.
class LocalModelsSection extends ConsumerWidget {
  const LocalModelsSection({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final text = Theme.of(context).textTheme;
    final hk = context.hk;
    final overview = ref.watch(localModelsProvider);
    final notifier = ref.read(localModelsProvider.notifier);
    void toast(String m) => ScaffoldMessenger.of(context)
      ..hideCurrentSnackBar()
      ..showSnackBar(SnackBar(content: Text(m)));
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            Expanded(child: Text('On-device models', style: text.titleMedium)),
            OutlinedButton.icon(
              icon: const Icon(Icons.upload_file, size: 18),
              label: const Text('Add GGUF file'),
              onPressed: () async {
                final r = await FilePicker.pickFiles(type: FileType.any);
                final path = r.firstOrNull?.path;
                if (path == null) return;
                final err = await notifier.addFile(path);
                if (context.mounted) toast(err ?? 'Model added');
              },
            ),
          ],
        ),
        const SizedBox(height: HkTokens.s1),
        Text(
          'Run models on this device: private, works offline, uses your CPU and RAM. Pick one as '
          'the offline fallback and your default model keeps working without a connection.',
          style: text.bodySmall?.copyWith(color: hk.textMuted),
        ),
        const SizedBox(height: HkTokens.s3),
        overview.when(
          loading: () => const Center(child: CircularProgressIndicator()),
          error: (e, _) => Text(describeError(e), style: TextStyle(color: hk.err)),
          data: (o) => Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              if (!o.engine)
                Padding(
                  padding: const EdgeInsets.only(bottom: HkTokens.s2),
                  child: Text(
                    'This build has no inference engine yet; models can be downloaded but not run.',
                    style: text.bodySmall?.copyWith(color: hk.warn),
                  ),
                ),
              Text('This device has ${formatBytes(o.totalRam)} of RAM.', style: text.bodySmall),
              const SizedBox(height: HkTokens.s2),
              Container(
                padding: const EdgeInsets.fromLTRB(12, 8, 4, 8),
                decoration: BoxDecoration(
                  color: hk.surface2,
                  borderRadius: BorderRadius.circular(HkTokens.radiusCard),
                  border: Border.all(color: hk.border),
                ),
                child: Row(
                  children: [
                    Icon(Icons.memory, size: 18, color: o.loaded.isEmpty ? hk.textFaint : hk.ok),
                    const SizedBox(width: HkTokens.s2),
                    Expanded(
                      child: Text(
                        o.loaded.isEmpty ? 'No model in memory' : 'In memory: ${o.loaded.join(', ')}',
                        style: text.bodySmall,
                      ),
                    ),
                    TextButton.icon(
                      onPressed: o.loaded.isEmpty
                          ? null
                          : () async {
                              await gate.hk('local.unload').catchError((_) => 0);
                              await notifier.refresh();
                              if (context.mounted) toast('Model unloaded; memory freed');
                            },
                      icon: const Icon(Icons.eject_outlined, size: 16),
                      label: const Text('Unload'),
                    ),
                  ],
                ),
              ),
              const SizedBox(height: HkTokens.s2),
              DropdownButtonFormField<String?>(
                key: ValueKey('fb-${o.fallback}-${o.installedChoices.length}'),
                initialValue: o.installedChoices.any((c) => c.$1 == o.fallback) ? o.fallback : null,
                isExpanded: true,
                decoration: const InputDecoration(labelText: 'Offline fallback model'),
                items: [
                  const DropdownMenuItem<String?>(value: null, child: Text('None')),
                  for (final c in o.installedChoices) DropdownMenuItem<String?>(value: c.$1, child: Text(c.$2)),
                ],
                onChanged: (v) async {
                  final err = await notifier.setFallback(v);
                  if (err != null && context.mounted) toast(err);
                },
              ),
              const SizedBox(height: HkTokens.s2),
              _GpuCard(overview: o, onChanged: (en, dev, layers) async {
                final err = await notifier.setGpu(enabled: en, device: dev, layers: layers);
                if (err != null && context.mounted) toast(err);
              }),
              const SizedBox(height: HkTokens.s3),
              for (final m in o.models)
                _ModelRow(
                  model: m,
                  recommended: m.id == o.recommended,
                  progress: notifier.progress[m.id],
                  onDownload: () async {
                    final err = await notifier.download(m.id, m.sizeBytes);
                    if (context.mounted && err != null) toast(err);
                  },
                  onCancel: () => notifier.cancel(m.id),
                  onDelete: () async {
                    final err = await notifier.delete(m.id);
                    if (context.mounted && err != null) toast(err);
                  },
                ),
              for (final c in o.custom)
                ListTile(
                  dense: true,
                  leading: const Icon(Icons.folder_open_outlined),
                  title: Text('${c['name']}'),
                  subtitle: const Text('Your file'),
                  trailing: IconButton(
                    tooltip: 'Forget (the file stays)',
                    icon: const Icon(Icons.close),
                    onPressed: () => notifier.delete('${c['id']}'),
                  ),
                ),
            ],
          ),
        ),
      ],
    );
  }
}

class _ModelRow extends StatelessWidget {
  const _ModelRow({
    required this.model,
    required this.recommended,
    required this.progress,
    required this.onDownload,
    required this.onCancel,
    required this.onDelete,
  });
  final LocalModel model;
  final bool recommended;
  final double? progress;
  final VoidCallback onDownload, onCancel, onDelete;

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final fitColor = switch (model.fit) {
      'good' => hk.ok,
      'tight' => hk.warn,
      _ => hk.err,
    };
    final fitLabel = switch (model.fit) {
      'good' => 'Runs well',
      'tight' => 'Tight on RAM',
      _ => 'Too large for this device',
    };
    final busy = progress != null || model.downloading;
    return Container(
      margin: const EdgeInsets.only(bottom: HkTokens.s2),
      padding: const EdgeInsets.all(HkTokens.s3),
      decoration: BoxDecoration(
        color: hk.surface,
        border: Border.all(color: recommended ? hk.accent : hk.border),
        borderRadius: BorderRadius.circular(HkTokens.radiusCard),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  '${model.name}${recommended ? '  · recommended' : ''}',
                  style: text.titleSmall,
                ),
              ),
              Text(formatBytes(model.sizeBytes), style: text.bodySmall?.copyWith(color: hk.textMuted)),
            ],
          ),
          Text(model.note, style: text.bodySmall?.copyWith(color: hk.textMuted)),
          const SizedBox(height: HkTokens.s1),
          Row(
            children: [
              Icon(Icons.memory, size: 14, color: fitColor),
              const SizedBox(width: HkTokens.s1),
              Expanded(
                child: Text(
                  '$fitLabel · needs about ${formatBytes(model.ramNeeded)} RAM · ${model.license}',
                  style: text.labelSmall?.copyWith(color: fitColor),
                ),
              ),
              if (busy)
                TextButton(onPressed: onCancel, child: const Text('Cancel'))
              else if (model.installed)
                TextButton.icon(onPressed: onDelete, icon: const Icon(Icons.delete_outline, size: 18), label: const Text('Delete'))
              else
                FilledButton.tonal(
                  onPressed: model.fit == 'too_big' ? null : onDownload,
                  child: const Text('Download'),
                ),
            ],
          ),
          if (busy)
            Padding(
              padding: const EdgeInsets.only(top: HkTokens.s2),
              child: LinearProgressIndicator(value: progress == 0 ? null : progress),
            ),
        ],
      ),
    );
  }
}

/// GPU offload for local models: on/off, which card, how many layers.
class _GpuCard extends StatefulWidget {
  const _GpuCard({required this.overview, required this.onChanged});

  final LocalOverview overview;
  final Future<void> Function(bool enabled, int device, int? layers) onChanged;

  @override
  State<_GpuCard> createState() => _GpuCardState();
}

class _GpuCardState extends State<_GpuCard> {
  late final _layers = TextEditingController(text: (widget.overview.gpu['layers'] as num?)?.toString() ?? '');

  @override
  void dispose() {
    _layers.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final o = widget.overview;
    final hk = context.hk;
    final text = Theme.of(context).textTheme;
    final backend = o.gpuBackend;
    if (backend == null) {
      return Text('Local models run on the CPU in this build.', style: text.bodySmall?.copyWith(color: hk.textMuted));
    }
    final enabled = o.gpu['enabled'] != false;
    final device = (o.gpu['device'] as num?)?.toInt() ?? 0;
    int? layers() => int.tryParse(_layers.text.trim());
    return Card(
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(HkTokens.s3),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              title: Text('Use the GPU (${backend[0].toUpperCase()}${backend.substring(1)})'),
              subtitle: const Text('Much faster when the model fits in video memory. If loading on the GPU fails, the CPU is used.'),
              value: enabled,
              onChanged: (v) => widget.onChanged(v, device, layers()),
            ),
            if (enabled)
              Row(
                children: [
                  SizedBox(
                    width: 280,
                    child: DropdownButtonFormField<int>(
                      initialValue: o.gpuDevices.any((d) => d['position'] == device) ? device : 0,
                      isExpanded: true,
                      decoration: const InputDecoration(labelText: 'GPU', isDense: true),
                      items: [
                        const DropdownMenuItem(value: 0, child: Text('Auto (best discrete GPU)')),
                        for (final d in o.gpuDevices)
                          DropdownMenuItem(
                            value: (d['position'] as num).toInt(),
                            child: Text(
                              '${d['description'] ?? d['name']}${d['integrated'] == true ? ' (integrated)' : ''}',
                              overflow: TextOverflow.ellipsis,
                            ),
                          ),
                      ],
                      onChanged: (v) => widget.onChanged(enabled, v ?? 0, layers()),
                    ),
                  ),
                  const SizedBox(width: HkTokens.s3),
                  Expanded(
                    child: TextField(
                      controller: _layers,
                      keyboardType: TextInputType.number,
                      decoration: const InputDecoration(labelText: 'Layers on the GPU', hintText: 'empty = all', isDense: true),
                      onSubmitted: (_) => widget.onChanged(enabled, device, layers()),
                    ),
                  ),
                  IconButton(tooltip: 'Apply', icon: const Icon(Icons.check), onPressed: () => widget.onChanged(enabled, device, layers())),
                ],
              ),
          ],
        ),
      ),
    );
  }
}
