import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../../theme/tokens.dart';
import 'player_controller.dart';
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

  static const _labelW = 72.0;
  static const _rowH = 26.0;

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
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
                  label: 'cut ${i + 1}',
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
                  tip: '${broll[i].query} — tap to remove',
                  onTap: () {
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
        Row(
          children: [
            TextButton.icon(
              onPressed: () {
                final src = plan.clock.toSource(widget.player.outTime.value);
                ctl.edit([
                  {'op': 'split_clip', 'clip': clip.id, 'at': src},
                ]);
              },
              icon: const Icon(Icons.content_cut, size: 16),
              label: const Text('Split at playhead'),
            ),
            const Spacer(),
            const Icon(Icons.zoom_out, size: 16),
            SizedBox(
              width: 140,
              child: Slider(
                value: _pxPerSec,
                min: 4,
                max: 120,
                onChanged: (v) => setState(() => _pxPerSec = v),
              ),
            ),
            const Icon(Icons.zoom_in, size: 16),
          ],
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
                  for (final n in const [
                    'Video',
                    'Captions',
                    'B-roll',
                    'Music',
                  ])
                    SizedBox(
                      height: _rowH,
                      child: Align(
                        alignment: Alignment.centerLeft,
                        child: Text(
                          n,
                          style: TextStyle(fontSize: 11, color: hk.textMuted),
                        ),
                      ),
                    ),
                ],
              ),
            ),
            Expanded(
              child: SingleChildScrollView(
                controller: _scroll,
                scrollDirection: Axis.horizontal,
                child: GestureDetector(
                  behavior: HitTestBehavior.translucent,
                  onTapDown: (d) => _seekAt(d.localPosition.dx),
                  onHorizontalDragUpdate: (d) => _seekAt(d.localPosition.dx),
                  child: SizedBox(
                    width: width,
                    child: Stack(
                      children: [
                        tracks,
                        ValueListenableBuilder<double>(
                          valueListenable: widget.player.outTime,
                          builder: (context, t, _) => Positioned(
                            left: t * _pxPerSec,
                            top: 0,
                            bottom: 0,
                            child: Container(width: 2, color: hk.err),
                          ),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
      ],
    );
  }
}
