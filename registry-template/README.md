# hokkabaz-plugins

Plugin registry for Hokkabaz. The app reads `index.json` from this repo's `main` branch
(`https://raw.githubusercontent.com/<owner>/hokkabaz-plugins/main/index.json`); artifacts are
GitHub Release assets. Nothing installs unless the download hashes to the `sha256` listed here.

## Publish a plugin

1. In your plugin repo: `hk plugin build` and attach `dist/<id>-<version>.tar.gz` to a GitHub Release.
2. `hk plugin index-entry dist/<id>-<version>.tar.gz --url <release asset url>` prints the entry.
3. Open a PR here adding (or replacing) the entry in `index.json`. CI runs
   `hk plugin verify-index index.json --download`: schema, https URL, hash, and that the archive's
   `plugin.toml` (id, version, capabilities) matches the entry.

Reviewers: read the `capabilities` block. It is what users are asked to approve.

## Publish a template

Templates (flows, agents, prompts, studio projects, settings presets, bundles) live in the same
`index.json` under `templates`. Format: `docs/TEMPLATES.md` in the Hokkabaz repo; examples in
`templates/`.

1. Pack the template directory from its root: `tar czf <id>-<version>.tar.gz -C templates/<dir> .`
   and attach it to a GitHub Release.
2. Add an entry `{id, name, version, kind, description, tags, author, min_hokkabaz_version,
   download_url, sha256, size}` to `templates` (`sha256sum` / `stat -c %s` of the archive).
   Fields must match the archive's `template.toml`.

Reviewers: check the declared `inputs`, especially `credential` inputs, and that payloads contain
no secrets.
