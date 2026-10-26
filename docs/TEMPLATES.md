# Templates

Templates (D19) are registry items that install a flow, agent, prompt, studio project, settings
preset, or a bundle of other templates. They share the plugin registry (D13, `registry-template/`):
same `index.json` (key `templates`), same id/version rules, same sha256-verified `.tar.gz` from
GitHub Releases. Code: `crates/hk-plugin/src/template.rs`.

## Package

```
template.toml
flow.json            # payload files named in `payload`
```

```toml
id = "dev.hokkabaz.webhook-to-telegram"   # [a-z0-9._-], like plugin ids
name = "Webhook to Telegram"
version = "1.0.0"                         # MAJOR.MINOR.PATCH
kind = "flow"                             # flow | agent | prompt | studio | settings_preset | bundle
description = "..."
tags = ["telegram"]
author = "..."
min_hokkabaz_version = "0.1.0"            # optional
payload = ["flow.json"]                   # bundles: none
# bundle_items = [{ id = "dev.x.other", min_version = "1.0.0" }]   # bundles only

[[inputs]]
key = "bot"                               # [A-Za-z][A-Za-z0-9_]*, unique
label = "Telegram bot"
kind = "credential"
credential_type = "telegramApi"           # n8n credential type
required = true
help = "..."
```

Input kinds: `credential` (`credential_type`), `model` (`capabilities = ["tools", ...]`), `string`,
`number`, `bool`, `select` (`options = [...]`), `file`. An optional `default` must fit the kind.

Payload checks by kind:

| kind | payload |
|---|---|
| flow | `.json`, n8n workflow export (parsed and validated by `hk-flow`) |
| agent | `.json` object with non-empty `name` (e.g. `model`, `mode`, `system`, `max_steps`) |
| studio, settings_preset | `.json` object |
| prompt | any UTF-8 text (`.md`, `.txt`, `.json`) |
| bundle | no payload; `bundle_items` reference other templates in the index |

## Placeholders

Payload strings use `{{inputs.<key>}}` (spaces inside the braces allowed). A string that is exactly
one placeholder becomes the input's typed value (`"{{inputs.max_steps}}"` -> `30`); otherwise the
value is spliced in as text. Other `{{ ... }}`, such as n8n expressions, are untouched. Every
placeholder must name a declared input.

Rendering (`TemplatePackage::render`) fails on a missing required input, an undeclared input key,
or a value of the wrong kind. Optional inputs left empty render as `null` (whole string) or `""`
(spliced). Credential inputs hold a stored credential's id; secrets never enter a template or its
output. Bundles render to their `bundle_items`; the installer resolves each from the index and
renders it with the bundle's inputs.

## Publish

See `registry-template/README.md`. Examples: `registry-template/templates/`.
