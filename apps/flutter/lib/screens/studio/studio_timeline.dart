import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'player_controller.dart';
import 'studio_models.dart' show formatTime;
import 'studio_providers.dart';

/// Multi-track timeline of the selected clip (output time): video segments, captions,
/// B-roll and music, with playhead, zoom, split-at-playhead and B-roll removal.
class MultiTrackTimeline extends ConsumerStatefulWidget {
  const MultiTrackTimeline({
    super.key,
    required this.projectId,
    required this.player,
  });
  final String projectId;
  final EdlPlayerController player;

  @override
  ConsumerState<MultiTrackTimeline> createState() => _MultiTrackTimelineState();
}

class _MultiTrackTimelineState extends ConsumerState<MultiTrackTimeline> {
  double _pxPerSec = 24;
  final _scroll = ScrollController();
  double _viewW = 800;

  static const _labelW = 84.0;
  static const _rowH = 28.0;

  @override
  void initState() {
    super.initState();
    widget.player.outTime.addListener(_follow);
  }

  @override
  void dispose() {
    widget.player.outTime.removeListener(_follow);
    _scroll.dispose();
    super.dispose();
  }

  /// Keep the playhead in view while playing.
  void _follow() {
    if (!widget.player.playing || !_scroll.hasClients) return;
    final x = widget.player.outTime.value * _pxPerSec;
    final o = _scroll.offset;
    if (x < o || x > o + _viewW - 40) {
      _scroll.jumpTo((x - 40).clamp(0.0, _scroll.position.maxScrollExtent));
    }
  }

  void _fit(double total) {
    if (total <= 0) return;
    setState(() => _pxPerSec = ((_viewW - 16) / total).clamp(4.0, 120.0));
  }

  void _seekAt(double dx) {
    final t = (dx / _pxPerSec).clamp(0.0, widget.player.duration);
    widget.player.seekOut(t);
  }

  @override
  Widget build(BuildContext context) {
    final s = ref.watch(studioControllerProvider(widget.projectId));
    final ctl = ref.read(studioControllerProvider(widget.projectId).notifier);
    final plan = s.plan;
    final clip = s.clip;
    final hk = context.hk;
    if (plan == null || clip == null) return const SizedBox.shrink();
    final total = plan.duration;
    final width = (total * _pxPerSec).clamp(200.0, 200000.0);

    // Output-time spans of the kept source ranges.
    final segs = <(double, double)>[];
    var acc = 0.0;
    for (final (a, b) in plan.ranges) {
      segs.add((acc, acc + (b - a)));
      acc += b - a;
    }

    Widget block(
      double start,
      double end,
      Color color, {
      String? label,
      VoidCallback? onTap,
      String? tip,
    }) {
      final w = ((end - start) * _pxPerSec).clamp(2.0, width);
      final child = Container(
        width: w,
        height: _rowH - 6,
        margin: const EdgeInsets.symmetric(vertical: 3),
        padding: const EdgeInsets.symmetric(horizontal: 4),
        alignment: Alignment.centerLeft,
        decoration: BoxDecoration(
          color: color,
          borderRadius: BorderRadius.circular(4),
        ),
        child: label == null || w < 24
            ? null
            : Text(
                label,
                maxLines: 1,
                overflow: TextOverflow.clip,
                style: const TextStyle(fontSize: 10, color: Colors.white),
              ),
      );
      return Positioned(
        left: start * _pxPerSec,
        top: 0,
        child: Tooltip(
          message: tip ?? label ?? '',
          child: GestureDetector(onTap: onTap, child: child),
        ),
      );
    }

    final broll = plan.broll;
    final tracks = Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // Ruler.
        SizedBox(
          height: 18,
          child: Stack(
            children: [
              for (
                var t = 0;
                t <= total;
                t += _pxPerSec >= 40 ? 1 : (_pxPerSec >= 15 ? 5 : 10)
              )
                Positioned(
                  left: t * _pxPerSec,
                  child: Text(
                    '${t ~/ 60}:${(t % 60).toString().padLeft(2, '0')}',
                    style: TextStyle(fontSize: 9, color: hk.textMuted),
                  ),
                ),
            ],
          ),
        ),
        SizedBox(
          height: _rowH,
          child: Stack(
            children: [
              for (var i = 0; i < segs.length; i++)
                block(
                  segs[i].$1,
                  segs[i].$2,
                  i.isEven
                      ? HkTokens.nodeIntegration
                      : HkTokens.nodeIntegration.withValues(alpha: 0.7),
                  label: segs.length == 1 ? clip.title : 'Part ${i + 1}',
                ),
            ],
          ),
        ),
        SizedBox(
          height: _rowH,
          child: Stack(
            children: [
              for (final w in plan.captions)
                block(
                  w.start,
                  w.end,
                  HkTokens.nodeAi.withValues(alpha: 0.85),
                  label: w.text,
                ),
            ],
          ),
        ),
        SizedBox(
          height: _rowH,
          child: Stack(
            children: [
              for (var i = 0; i < broll.length; i++)
                block(
                  broll[i].start,
                  broll[i].end,
                  HkTokens.nodeData,
                  label: broll[i].query,
                  tip: '${broll[i].query} — click for options',
                  onTap: () async {
                    final b = broll[i];
                    final box = context.findRenderObject() as RenderBox?;
                    final pos =
                        box?.localToGlobal(
                          Offset(
                            _labelW + b.start * _pxPerSec - _scroll.offset,
                            0,
                          ),
                        ) ??
                        Offset.zero;
                    final v = await showMenu<String>(
                      context: context,
                      position: RelativeRect.fromLTRB(
                        pos.dx,
                        pos.dy,
                        pos.dx,
                        pos.dy,
                      ),
                      items: const [
                        PopupMenuItem(value: 'seek', child: Text('Jump here')),
                        PopupMenuItem(
                          value: 'remove',
                          child: Text('Remove B-roll'),
                        ),
                      ],
                    );
                    if (v == 'seek') widget.player.seekOut(b.start);
                    if (v == 'remove') {
                      final rest = [
                        for (var k = 0; k < broll.length; k++)
                          if (k != i)
                            {
                              'start': broll[k].start,
                              'end': broll[k].end,
                              'query': broll[k].query,
                              'asset': broll[k].asset,
                              'kind': broll[k].isImage ? 'image' : 'video',
                            },
                      ];
                      ctl.edit([
                        {'op': 'set_broll', 'clip': clip.id, 'broll': rest},
                      ]);
                    }
                  },
                ),
            ],
          ),
        ),
        SizedBox(
          height: _rowH,
          child: Stack(
            children: [
              if (clip.music != null)
                block(0, total, HkTokens.nodeTrigger, label: 'music'),
            ],
          ),
        ),
      ],
    );

    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        SizedBox(
          height: 40,
          child: Row(
            children: [
              ListenableBuilder(
                listenable: widget.player,
                builder: (context, _) => IconButton(
                  tooltip: widget.player.playing
                      ? 'Pause (Space)'
                      : 'Play (Space)',
                  icon: Icon(
                    widget.player.playing
                        ? Icons.pause_rounded
                        : Icons.play_arrow_rounded,
                  ),
                  onPressed: widget.player.toggle,
                ),
              ),
              ValueListenableBuilder<double>(
                valueListenable: widget.player.outTime,
                builder: (context, t, _) => Text(
                  '${formatTime(t)} / ${formatTime(total)}',
                  style: TextStyle(
                    fontSize: 12,
                    fontFamily: HkTokens.fontMono,
                    color: hk.textMuted,
                  ),
                ),
              ),
              const SizedBox(width: HkTokens.s3),
              Container(width: 1, height: 20, color: hk.border),
              const SizedBox(width: HkTokens.s2),
              TextButton.icon(
                style: TextButton.styleFrom(foregroundColor: hk.text),
                onPressed: () {
                  final src = plan.clock.toSource(widget.player.outTime.value);
                  ctl.edit([
                    {'op': 'split_clip', 'clip': clip.id, 'at': src},
                  ]);
                },
                icon: const Icon(Icons.content_cut, size: 16),
                label: const Text('Split'),
              ),
              const Spacer(),
              TextButton(
                style: TextButton.styleFrom(foregroundColor: hk.textMuted),
                onPressed: () => _fit(total),
                child: const Text('Fit'),
              ),
              IconButton(
                tooltip: 'Zoom out',
                icon: const Icon(Icons.zoom_out, size: 18),
                onPressed: () => setState(
                  () => _pxPerSec = (_pxPerSec / 1.4).clamp(4.0, 120.0),
                ),
              ),
              SizedBox(
                width: 120,
                child: Slider(
                  value: _pxPerSec,
                  min: 4,
                  max: 120,
                  onChanged: (v) => setState(() => _pxPerSec = v),
                ),
              ),
              IconButton(
                tooltip: 'Zoom in',
                icon: const Icon(Icons.zoom_in, size: 18),
                onPressed: () => setState(
                  () => _pxPerSec = (_pxPerSec * 1.4).clamp(4.0, 120.0),
                ),
              ),
            ],
          ),
        ),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            SizedBox(
              width: _labelW,
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                children: [
                  const SizedBox(height: 18),
                  for (final (n, i) in const [
                    ('Video', Icons.videocam_outlined),
                    ('Captions', Icons.closed_caption_outlined),
                    ('B-roll', Icons.collections_outlined),
                    ('Music', Icons.music_note_outlined),
                  ])
                    SizedBox(
                      height: _rowH,
                      child: Row(
                        children: [
                          Icon(i, size: 14, color: hk.textFaint),
                          const SizedBox(width: 6),
                          Flexible(
                            child: Text(
                              n,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: TextStyle(
                                fontSize: 11,
                                color: hk.textMuted,
                              ),
                            ),
                          ),
                        ],
                      ),
                    ),
                ],
              ),
            ),
            Expanded(
              child: LayoutBuilder(
                builder: (context, box) {
                  _viewW = box.maxWidth;
                  return SingleChildScrollView(
                    controller: _scroll,
                    scrollDirection: Axis.horizontal,
                    child: GestureDetector(
                      behavior: HitTestBehavior.translucent,
                      onTapDown: (d) => _seekAt(d.localPosition.dx),
                      onHorizontalDragUpdate: (d) =>
                          _seekAt(d.localPosition.dx),
                      child: SizedBox(
                        width: width < _viewW ? _viewW : width,
                        child: Stack(
                          children: [
                            for (var r = 0; r < 4; r++)
                              Positioned(
                                left: 0,
                                right: 0,
                                top: 18 + r * _rowH + 2,
                                height: _rowH - 4,
                                child: DecoratedBox(
                                  decoration: BoxDecoration(
                                    color: hk.surface2,
                                    borderRadius: BorderRadius.circular(4),
                                  ),
                                ),
                              ),
                            tracks,
                            ValueListenableBuilder<double>(
                              valueListenable: widget.player.outTime,
                              builder: (context, t, _) => Positioned(
                                left: t * _pxPerSec,
                                top: 0,
                                bottom: 0,
                                child: Container(width: 2, color: hk.accent2),
                              ),
                            ),
                          ],
                        ),
                      ),
                    ),
                  );
                },
              ),
            ),
          ],
        ),
      ],
    );
  }
}
