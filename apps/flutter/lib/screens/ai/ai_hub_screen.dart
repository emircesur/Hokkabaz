import 'package:flutter/material.dart';

import '../../app/activity.dart';
import '../../theme/tokens.dart';
import 'sections_library.dart';
import 'sections_settings.dart';
import 'sections_tools.dart';

/// AI hub: templates, agents, prompts, memory, knowledge, arena, usage, schedules,
/// context, checkpoints, generation and AI settings (Phase 9).
class AiHubScreen extends StatefulWidget {
  const AiHubScreen({super.key});
  @override
  State<AiHubScreen> createState() => _AiHubScreenState();
}

class _AiHubScreenState extends State<AiHubScreen> {
  int _i = 0;

  static final _sections = <(String, IconData, Widget Function())>[
    (
      'Templates',
      Icons.dashboard_customize_outlined,
      () => const TemplatesSection(),
    ),
    ('Agents', Icons.smart_toy_outlined, () => const AgentsSection()),
    ('Prompts', Icons.notes, () => const PromptsSection()),
    ('Memory', Icons.psychology_outlined, () => const MemorySection()),
    ('Knowledge', Icons.menu_book_outlined, () => const KnowledgeSection()),
    ('Arena', Icons.compare_arrows, () => const ArenaSection()),
    ('Usage', Icons.insights_outlined, () => const UsageSection()),
    ('Schedules', Icons.event_repeat, () => const SchedulesSection()),
    ('Context', Icons.data_object, () => const ContextSection()),
    ('Checkpoints', Icons.history, () => const CheckpointsSection()),
    ('Generate', Icons.auto_awesome_outlined, () => const GenerateSection()),
    ('Activity', Icons.timelapse, () => const ActivityPanel()),
    ('AI settings', Icons.tune, () => const AiSettingsSection()),
  ];

  @override
  Widget build(BuildContext context) {
    final hk = context.hk;
    final narrow = MediaQuery.sizeOf(context).width < 800;
    final body = KeyedSubtree(key: ValueKey(_i), child: _sections[_i].$3());
    if (narrow) {
      return Column(
        children: [
          SizedBox(
            height: 48,
            child: ListView(
              scrollDirection: Axis.horizontal,
              padding: const EdgeInsets.symmetric(horizontal: HkTokens.s2),
              children: [
                for (var k = 0; k < _sections.length; k++)
                  Padding(
                    padding: const EdgeInsets.only(right: 4, top: 6),
                    child: ChoiceChip(
                      avatar: Icon(_sections[k].$2, size: 16),
                      label: Text(_sections[k].$1),
                      selected: _i == k,
                      onSelected: (_) => setState(() => _i = k),
                    ),
                  ),
              ],
            ),
          ),
          Expanded(child: body),
        ],
      );
    }
    return Row(
      children: [
        SizedBox(
          width: 200,
          child: ListView(
            padding: const EdgeInsets.symmetric(vertical: HkTokens.s3),
            children: [
              Padding(
                padding: const EdgeInsets.fromLTRB(
                  HkTokens.s4,
                  0,
                  HkTokens.s4,
                  HkTokens.s2,
                ),
                child: Text(
                  'AI hub',
                  style: Theme.of(context).textTheme.titleLarge,
                ),
              ),
              for (var k = 0; k < _sections.length; k++)
                ListTile(
                  dense: true,
                  selected: _i == k,
                  selectedTileColor: hk.accent.withValues(alpha: 0.12),
                  leading: Icon(_sections[k].$2, size: 20),
                  title: Text(_sections[k].$1),
                  onTap: () => setState(() => _i = k),
                ),
            ],
          ),
        ),
        VerticalDivider(width: 1, color: hk.border),
        Expanded(child: body),
      ],
    );
  }
}
