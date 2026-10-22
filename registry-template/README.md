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
