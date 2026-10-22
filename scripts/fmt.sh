#!/usr/bin/env bash
# rustfmt the workspace members only. `cargo fmt --all` also reformats path dependencies, which
# includes the third_party/concat submodule (not ours to touch).
# usage: scripts/fmt.sh [--check]
set -euo pipefail
cd "$(dirname "$0")/.."
pkgs=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(" ".join("-p "+p["name"] for p in json.load(sys.stdin)["packages"]))')
# shellcheck disable=SC2086
cargo fmt $pkgs "$@"
