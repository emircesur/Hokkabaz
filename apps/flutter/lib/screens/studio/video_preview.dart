import 'package:flutter/material.dart';

import '../../theme/tokens.dart';
import 'caption_overlay.dart';
import 'player_backends.dart';
import 'player_controller.dart';
import 'studio_models.dart';

/// The live preview: source video cropped by the reframing path, captions on
/// top, exactly as the render will look. Edits show up the moment the plan changes.
class VideoPreview extends StatelessWidget {
  const VideoPreview({super.key, required this.controller, required this.plan, required this.source});

  final EdlPlayerController controller;
  final PreviewPlan? plan;
  final StudioSource source;

  @override
  Widget build(BuildContext context) {
    final p = plan;
    final aspect = p == null || p.crop == null ? source.width / (source.height == 0 ? 1 : source.height) : p.aspect;
    final lines = groupCaptionLines(p?.captions ?? const [], p?.captionStyle);
    return Center(
      child: AspectRatio(
        aspectRatio: aspect <= 0 ? 16 / 9 : aspect,
        child: ClipRRect(
          borderRadius: BorderRadius.circular(HkTokens.radiusCard),
          child: ColoredBox(
            color: Colors.black,
            child: Stack(
              fit: StackFit.expand,
              children: [
                _CroppedVideo(controller: controller, crop: p?.crop, source: source),
                CaptionOverlay(lines: lines, style: p?.captionStyle, time: controller.outTime),
              ],
            ),
          ),
        ),
      ),
    );
  }
}

class _CroppedVideo extends StatelessWidget {
  const _CroppedVideo({required this.controller, required this.crop, required this.source});

  final EdlPlayerController controller;
  final CropPath? crop;
  final StudioSource source;

  @override
  Widget build(BuildContext context) {
    final view = backendView(controller.backend);
    final path = crop;
    if (path == null || path.keys.isEmpty || source.width == 0) {
      return FittedBox(fit: BoxFit.contain, child: SizedBox(width: source.width.toDouble().clamp(2, 1e5), height: source.height.toDouble().clamp(2, 1e5), child: view));
    }
    return LayoutBuilder(
      builder: (context, box) => ValueListenableBuilder<double>(
        valueListenable: controller.outTime,
        builder: (context, t, _) {
          final k = path.at(t);
          if (k.w <= 0 || k.h <= 0) return view;
          // Scale so the crop window fills the box, then shift the source under it.
          final s = box.maxWidth / k.w;
          return ClipRect(
            child: OverflowBox(
              alignment: Alignment.topLeft,
              minWidth: 0,
              minHeight: 0,
              maxWidth: double.infinity,
              maxHeight: double.infinity,
              child: Transform.translate(
                offset: Offset(-k.x * s, -k.y * s),
                child: SizedBox(width: source.width * s, height: source.height * s, child: view),
              ),
            ),
          );
        },
      ),
    );
  }
}

/// Play / pause, time readout and scrubber over the output timeline.
class TransportBar extends StatelessWidget {
  const TransportBar({super.key, required this.controller});

  final EdlPlayerController controller;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: controller,
      builder: (context, _) {
        final d = controller.duration;
        return ValueListenableBuilder<double>(
          valueListenable: controller.outTime,
          builder: (context, t, _) => Row(
            children: [
              IconButton(
                tooltip: controller.playing ? 'Pause (Space)' : 'Play (Space)',
                onPressed: d == 0 ? null : controller.toggle,
                icon: Icon(controller.playing ? Icons.pause_circle_filled : Icons.play_circle_fill, size: 36),
              ),
              Text(formatTime(t), style: Theme.of(context).textTheme.bodySmall),
              Expanded(
                child: Slider(
                  value: d == 0 ? 0 : t.clamp(0.0, d),
                  max: d == 0 ? 1 : d,
                  onChanged: d == 0 ? null : controller.seekOut,
                ),
              ),
              Text(formatTime(d), style: Theme.of(context).textTheme.bodySmall),
            ],
          ),
        );
      },
    );
  }
}
