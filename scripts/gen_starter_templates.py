# Generates crates/hk-ffi/assets/starter_templates.json (built-in template catalog).
import json, sys, uuid

def node(name, typ, params, ver=1, pos=0):
    return {"id": str(uuid.uuid5(uuid.NAMESPACE_URL, name+typ)), "name": name, "type": typ,
            "typeVersion": ver, "position": [pos*260, 0], "parameters": params}

def chain(name, nodes):
    conns = {}
    for a, b in zip(nodes, nodes[1:]):
        conns[a["name"]] = {"main": [[{"node": b["name"], "type": "main", "index": 0}]]}
    for i, n in enumerate(nodes):
        n["position"] = [i*260, 0]
    return {"name": name, "nodes": nodes, "connections": conns, "settings": {"executionOrder": "v1"}, "active": False}

S = "n8n-nodes-base."
L = "@n8n/n8n-nodes-langchain."
def cron(expr): return node("Schedule", S+"scheduleTrigger", {"rule": {"interval": [{"field": "cronExpression", "expression": expr}]}}, 1.2)
def hook(path, mode="onReceived"): return node("Webhook", S+"webhook", {"path": path, "httpMethod": "POST", "responseMode": mode}, 2)
def llm(name, prompt): return node(name, L+"chainLlm", {"promptType": "define", "text": prompt}, 1.5)
def agent(name, prompt): return node(name, "hokkabaz.agent", {"prompt": prompt, "model": "{{inputs.model}}", "mode": "build"}, 1)
def http(name, url, method="GET"): return node(name, S+"httpRequest", {"url": url, "method": method}, 4.2)
def tg(text): return node("Telegram", S+"telegram", {"chatId": "{{inputs.telegram_chat_id}}", "text": text}, 1.2)
def ntfy(text): return http("ntfy", "{{inputs.ntfy_url}}", "POST") | {"parameters": {"url": "{{inputs.ntfy_url}}", "method": "POST", "sendBody": True, "contentType": "raw", "body": text}}
def code(name, js): return node(name, S+"code", {"jsCode": js}, 2)
def setf(name, fields): return node(name, S+"set", {"assignments": {"assignments": [{"name": k, "value": v, "type": "string"} for k, v in fields.items()]}}, 3.4)
def respond(body): return node("Respond", S+"respondToWebhook", {"respondWith": "json", "responseBody": body}, 1.1)

MODEL = {"key": "model", "label": "Model", "kind": {"type": "model", "capabilities": ["tools"]}, "required": False, "default": "deepseek/deepseek-chat"}
TG = {"key": "telegram_chat_id", "label": "Telegram chat id", "kind": {"type": "string"}, "required": True}
NTFY = {"key": "ntfy_url", "label": "ntfy topic URL", "kind": {"type": "string"}, "required": True, "default": "https://ntfy.sh/hokkabaz"}
def s(key, label, default=None, req=True):
    d = {"key": key, "label": label, "kind": {"type": "string"}, "required": req}
    if default is not None: d["default"] = default
    return d

flows = [
 ("flow-rss-digest", "RSS → AI digest → Telegram", "Daily summary of new RSS items.", [s("feed_url","RSS feed URL"), TG, MODEL],
  [cron("0 8 * * *"), node("RSS", S+"rssFeedRead", {"url": "{{inputs.feed_url}}"}, 1.1), node("Top 10", S+"limit", {"maxItems": 10}, 1),
   node("Aggregate", S+"aggregate", {"aggregate": "aggregateAllItemData"}, 1), llm("Summarize", "Summarize these articles in 5 bullets:\n{{ JSON.stringify($json.data) }}"), tg("={{ $json.text }}")]),
 ("flow-daily-briefing", "Daily briefing agent", "Morning briefing: weather, news, calendar via agent tools.", [s("city","City","Istanbul"), TG, MODEL],
  [cron("30 7 * * 1-5"), agent("Briefing", "Prepare a short morning briefing for {{inputs.city}}: weather, top tech news, and a motivational line."), tg("={{ $json.output }}")]),
 ("flow-github-issue-triage", "GitHub issue triage", "Label and answer new issues with AI.", [s("repo","owner/repo"), MODEL],
  [hook("github-issues"), node("Classify", L+"textClassifier", {"inputText": "={{ $json.body.issue.title }}\n{{ $json.body.issue.body }}", "categories": {"categories": [{"category": "bug"}, {"category": "feature"}, {"category": "question"}]}}, 1),
   agent("Reply", "Draft a helpful first reply for this {{ $json.category }} issue in {{inputs.repo}}.")]),
 ("flow-gitea-release-notes", "Gitea/Forgejo release notes", "Generate release notes from merged PR titles.", [s("api_url","Gitea API base URL"), s("repo","owner/repo"), MODEL],
  [hook("release"), http("Pulls", "={{'{{inputs.api_url}}'}}/repos/{{inputs.repo}}/pulls?state=closed&limit=50"), llm("Notes", "Write grouped release notes (Features, Fixes, Other) from these PRs:\n{{ JSON.stringify($json) }}")]),
 ("flow-webhook-classify-route", "Webhook classify & route", "Classify incoming messages and route by category.", [MODEL],
  [hook("inbox", "lastNode"), node("Classify", L+"textClassifier", {"inputText": "={{ $json.body.text }}", "categories": {"categories": [{"category": "sales"}, {"category": "support"}, {"category": "spam"}]}}, 1), respond("={{ { category: $json.category } }}")]),
 ("flow-uptime-alert-summary", "Uptime alert summarizer", "Turn monitoring alerts into a readable ntfy push.", [NTFY, MODEL],
  [hook("uptime"), llm("Explain", "Explain this alert for an on-call engineer in 2 sentences:\n{{ JSON.stringify($json.body) }}"), ntfy("={{ $json.text }}")]),
 ("flow-email-draft-replies", "Email draft replies", "Read unread mail and draft replies.", [s("imap_host","IMAP host"), MODEL],
  [node("IMAP", S+"emailReadImap", {"mailbox": "INBOX", "postProcessAction": "nothing"}, 2), llm("Draft", "Draft a polite reply to:\nSubject: {{ $json.subject }}\n{{ $json.text }}")]),
 ("flow-site-change-monitor", "Website change monitor", "Alert when a page changes.", [s("url","Page URL"), NTFY],
  [cron("*/30 * * * *"), http("Fetch", "{{inputs.url}}"), code("Diff", "const h = require('crypto').createHash('sha1').update(JSON.stringify($input.first().json)).digest('hex');\nconst s = $getWorkflowStaticData('global');\nconst changed = s.h && s.h !== h; s.h = h;\nreturn changed ? [{json:{changed:true}}] : [];"), ntfy("{{inputs.url}} changed")]),
 ("flow-searxng-research", "SearXNG research digest", "Search the web via your SearXNG and summarize.", [s("searxng_url","SearXNG URL"), s("topic","Topic"), TG, MODEL],
  [cron("0 9 * * 1"), http("Search", "{{inputs.searxng_url}}/search?q={{inputs.topic}}&format=json"), llm("Digest", "Write a weekly research digest on {{inputs.topic}} from these results with links:\n{{ JSON.stringify($json.results) }}"), tg("={{ $json.text }}")]),
 ("flow-meeting-actions", "Meeting transcript → action items", "Extract owners and deadlines from a transcript.", [MODEL],
  [hook("meeting", "lastNode"), node("Extract", L+"informationExtractor", {"text": "={{ $json.body.transcript }}", "schemaType": "manual", "inputSchema": "{\"type\":\"object\",\"properties\":{\"actions\":{\"type\":\"array\",\"items\":{\"type\":\"object\",\"properties\":{\"owner\":{\"type\":\"string\"},\"task\":{\"type\":\"string\"},\"due\":{\"type\":\"string\"}}}}}}"}, 1), respond("={{ $json.output }}")]),
 ("flow-receipt-extract", "Receipt extraction → CSV row", "Extract vendor/total/date from receipt text.", [MODEL],
  [hook("receipt", "lastNode"), node("Extract", L+"informationExtractor", {"text": "={{ $json.body.text }}", "schemaType": "manual", "inputSchema": "{\"type\":\"object\",\"properties\":{\"vendor\":{\"type\":\"string\"},\"total\":{\"type\":\"number\"},\"date\":{\"type\":\"string\"}}}"}, 1), respond("={{ $json.output }}")]),
 ("flow-podcast-to-shorts", "Podcast → shorts", "New episode file → Studio batch: transcribe, find highlights, render shorts.", [s("folder","Episodes folder")],
  [node("Watch", "hokkabaz.fileWatchTrigger", {"path": "{{inputs.folder}}"}, 1), agent("Studio", "Use studio tools to make 3 vertical shorts from the new video at {{ $json.path }} with the shorts-karaoke template.")]),
 ("flow-youtube-social", "Video → social posts", "Write per-platform posts for a published video.", [MODEL],
  [hook("video-published", "lastNode"), llm("Posts", "Write a TikTok caption, an X post and a LinkedIn post for this video:\nTitle: {{ $json.body.title }}\n{{ $json.body.description }}"), respond("={{ { posts: $json.text } }}")]),
 ("flow-home-assistant-voice", "Home Assistant command agent", "Natural-language commands to Home Assistant.", [s("ha_url","Home Assistant URL"), MODEL],
  [hook("ha-command", "lastNode"), agent("HA", "Translate this request into a Home Assistant service call JSON (domain, service, entity_id): {{ $json.body.text }}"), respond("={{ $json.output }}")]),
 ("flow-backup-check", "Backup check", "Daily check that the latest backup exists and is fresh.", [s("status_url","Backup status endpoint"), NTFY],
  [cron("0 6 * * *"), http("Status", "{{inputs.status_url}}"), node("Stale?", S+"if", {"conditions": {"conditions": [{"leftValue": "={{ $json.age_hours }}", "rightValue": 26, "operator": {"type": "number", "operation": "gt"}}]}}, 2), ntfy("Backup is stale!")]),
 ("flow-pr-review-bot", "PR review bot", "Review PR diffs with AI and comment.", [s("repo","owner/repo"), MODEL],
  [hook("pull-request"), http("Diff", "={{ $json.body.pull_request.diff_url }}"), agent("Review", "Review this diff for bugs and risky changes; reply with concise review comments:\n{{ $json.data }}")]),
 ("flow-matrix-bot", "Matrix chat bot", "Answer messages in a Matrix room with AI.", [s("room_id","Room id"), MODEL],
  [hook("matrix"), llm("Answer", "Reply helpfully and briefly: {{ $json.body.text }}"), node("Matrix", S+"matrix", {"roomId": "{{inputs.room_id}}", "text": "={{ $json.text }}"}, 1)]),
 ("flow-slack-standup", "Slack standup summary", "Summarize standup thread into a digest.", [s("channel","Slack channel"), MODEL],
  [cron("0 11 * * 1-5"), llm("Digest", "Summarize today's standup updates into blockers, done, next."), node("Slack", S+"slack", {"channel": "{{inputs.channel}}", "text": "={{ $json.text }}"}, 2.2)]),
 ("flow-lead-enrichment", "Lead enrichment", "Enrich a new lead with company info via agent web research.", [MODEL],
  [hook("lead", "lastNode"), agent("Enrich", "Research the company of {{ $json.body.email }} and return JSON: company, size, industry, website."), respond("={{ $json.output }}")]),
 ("flow-support-faq", "Support FAQ answerer", "Answer customer questions from your knowledge base.", [s("kb","Knowledge base name","docs"), MODEL],
  [hook("faq", "lastNode"), agent("FAQ", "Use the knowledge tool (base {{inputs.kb}}) to answer with citations: {{ $json.body.question }}"), respond("={{ { answer: $json.output } }}")]),
 ("flow-translate-webhook", "Translation endpoint", "POST text, get a translation back.", [s("target","Target language","Turkish"), MODEL],
  [hook("translate", "lastNode"), llm("Translate", "Translate to {{inputs.target}}, keep formatting:\n{{ $json.body.text }}"), respond("={{ { text: $json.text } }}")]),
 ("flow-weekly-report", "Weekly metrics report", "Pull metrics JSON and write a narrative report.", [s("metrics_url","Metrics endpoint"), TG, MODEL],
  [cron("0 17 * * 5"), http("Metrics", "{{inputs.metrics_url}}"), llm("Report", "Write a weekly report with highlights and concerns from:\n{{ JSON.stringify($json) }}"), tg("={{ $json.text }}")]),
 ("flow-postgres-nl-query", "Natural language → SQL report", "Ask questions about your Postgres data.", [MODEL],
  [hook("ask-db", "lastNode"), llm("SQL", "Write one safe read-only PostgreSQL SELECT for: {{ $json.body.question }}. Reply with SQL only."), node("Postgres", S+"postgres", {"operation": "executeQuery", "query": "={{ $json.text }}"}, 2.5), respond("={{ $json }}")]),
 ("flow-redis-rate-alert", "Error spike alert", "Count errors per minute in Redis and alert on spikes.", [NTFY],
  [hook("error"), node("Count", S+"redis", {"operation": "incr", "key": "errors:{{ $now.toFormat('yyyyMMddHHmm') }}"}, 1), node("Spike?", S+"if", {"conditions": {"conditions": [{"leftValue": "={{ $json.value }}", "rightValue": 20, "operator": {"type": "number", "operation": "gt"}}]}}, 2), ntfy("Error spike")]),
 ("flow-nightly-agent", "Nightly agent report", "Run an agent task every night and push the result.", [s("task","Task for the agent","Check my repo for TODOs and summarize them."), NTFY, MODEL],
  [cron("0 2 * * *"), agent("Agent", "{{inputs.task}}"), ntfy("={{ $json.output }}")]),
 ("flow-chat-assistant", "Chat assistant with memory", "Chat trigger + agent with window memory.", [MODEL],
  [node("Chat", L+"chatTrigger", {}, 1.1), node("Agent", L+"agent", {"promptType": "auto"}, 1.7)]),
]

agents = [
 ("agent-coder", "Coder", "Writes and edits code with tests.", "You are a senior engineer. Make minimal, correct changes; run checks; explain briefly.", ["read","write","edit","glob","grep","bash","checkpoint"]),
 ("agent-reviewer", "Code reviewer", "Reviews diffs for bugs and risks.", "Review changes for correctness, security and clarity. Report findings ranked by severity.", ["read","glob","grep"]),
 ("agent-researcher", "Researcher", "Web research with citations.", "Research thoroughly, compare sources, cite links, flag uncertainty.", ["web_fetch","knowledge","memory"]),
 ("agent-devops", "DevOps", "Docker, CI and deployment helper.", "You manage containers, CI and deployments carefully; prefer dry runs and explain risks.", ["bash","read","edit","checkpoint"]),
 ("agent-analyst", "Data analyst", "SQL and CSV analysis.", "Analyze data step by step; show queries and summarize findings with numbers.", ["bash","read","write"]),
 ("agent-writer", "Writer", "Drafts posts, docs and emails.", "Write clearly in the user's voice; offer a short and a long version.", ["read","write","memory"]),
 ("agent-translator", "Translator", "Natural translations, keeps formatting.", "Translate faithfully and naturally; keep markdown and code untouched.", []),
 ("agent-video-editor", "Video editor", "Makes shorts with Studio tools.", "Use studio_* tools to find highlights, edit by text, caption, reframe and render shorts.", ["studio_projects","studio_transcribe","studio_highlights","studio_edit","studio_render","studio_writeup","studio_social"]),
 ("agent-social", "Social manager", "Plans and writes social posts.", "Plan posts per platform limits, hooks first, consistent brand voice.", ["studio_social","studio_writeup","memory"]),
 ("agent-support", "Support triage", "Answers from the knowledge base.", "Answer support questions from the knowledge base with citations; escalate when unsure.", ["knowledge","memory"]),
 ("agent-home", "Home automation", "Home Assistant helper.", "Turn requests into Home Assistant actions; confirm before acting on locks or heating.", ["web_fetch"]),
 ("agent-tutor", "Tutor", "Explains concepts step by step.", "Teach with questions, examples and short checks for understanding.", ["memory"]),
]

prompts = [
 ("prompt-summarize", "Summarize", "TL;DR + bullets", "Summarize the following in one TL;DR sentence and 5 bullets:\n\n{{text}}"),
 ("prompt-explain-code", "Explain code", "Line-by-line explanation", "Explain what this code does, then list edge cases:\n\n{{code}}"),
 ("prompt-commit-msg", "Commit message", "Conventional commit", "Write a conventional commit message for this diff:\n\n{{diff}}"),
 ("prompt-email-reply", "Email reply", "Polite reply", "Draft a polite, concise reply to this email:\n\n{{email}}"),
 ("prompt-tweet-thread", "Thread", "5-post thread", "Turn this into a 5-post thread with a strong hook:\n\n{{text}}"),
]

presets = [
 ("preset-precise-coder", "Precise coder", {"temperature": 0.1, "top_p": 0.9, "reasoning_effort": "high"}),
 ("preset-creative-writer", "Creative writer", {"temperature": 0.9, "top_p": 0.95, "presence_penalty": 0.4}),
 ("preset-cheap-fast", "Cheap & fast", {"temperature": 0.3, "max_output_tokens": 1024, "reasoning_effort": "low"}),
 ("preset-offline-phone", "Offline phone", {"temperature": 0.4, "max_output_tokens": 512, "context_limit": 4096}),
]

studio = [
 ("podcast-clip", "Podcast clip", "classic", "split", True), ("gaming-highlight", "Gaming highlight", "pop", "center", True),
 ("tutorial", "Tutorial", "block", "center", False), ("interview-split", "Interview split-screen", "classic", "speaker", True),
 ("news-brief", "News brief", "block", "center", True), ("reaction", "Reaction", "pop", "speaker", True),
 ("quote-card", "Quote card", "block", "center", False), ("listicle", "Listicle", "pop", "center", True),
 ("meme", "Meme caption", "block", "center", False), ("before-after", "Before / after", "classic", "center", False),
 ("motivation", "Motivation", "karaoke", "speaker", True), ("product-demo", "Product demo", "classic", "center", False),
 ("lecture", "Lecture highlight", "block", "speaker", True), ("vlog", "Vlog", "karaoke", "speaker", True),
 ("square-quote", "Square quote", "block", "center", False),
]

out = []
for id_, name, desc, inputs, nodes in flows:
    out.append({"id": id_, "name": name, "kind": "flow", "version": "1.0.0", "description": desc, "tags": ["starter"], "inputs": inputs, "payload": chain(name, nodes)})
for id_, name, desc, prompt, tools in agents:
    out.append({"id": id_, "name": name, "kind": "agent", "version": "1.0.0", "description": desc, "tags": ["agent"], "inputs": [MODEL],
                "payload": {"id": id_, "name": name, "system_prompt": prompt, "tools": tools, "model": "{{inputs.model}}", "mode": "build"}})
for id_, name, desc, text in prompts:
    out.append({"id": id_, "name": name, "kind": "prompt", "version": "1.0.0", "description": desc, "tags": ["prompt"], "inputs": [],
                "payload": {"id": id_, "name": name, "text": text}})
for id_, name, st in presets:
    out.append({"id": id_, "name": name, "kind": "settings_preset", "version": "1.0.0", "description": f"AI settings preset: {name}", "tags": ["preset"], "inputs": [],
                "payload": {"id": id_, "name": name, "settings": st}})
# Studio templates ship as hk-studio built-ins (templates.rs); listed here for the gallery only.
for id_, name, cap, mode, fill in []:
    out.append({"id": "studio-"+id_, "name": name, "kind": "studio", "version": "1.0.0", "description": f"Studio template: {name}", "tags": ["studio"], "inputs": [],
                "payload": {"name": id_, "description": name, "captions_preset": cap, "reframe_mode": mode, "remove_fillers": fill}})
json.dump(out, open(sys.argv[1], "w"), indent=0, ensure_ascii=False)
print(len(flows), len(agents), len(prompts), len(presets), len(studio), len(out))
