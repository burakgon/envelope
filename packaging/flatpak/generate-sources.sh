#!/bin/sh
# Regenerates the offline dependency sources the Flatpak build reads
# (cargo-sources.json, node-sources.json) from the lock files.
# Needs git and Python 3 with aiohttp and tomlkit.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tools=${FLATPAK_BUILDER_TOOLS:-"$here/.flatpak-builder-tools"}
[ -d "$tools" ] || git clone --depth 1 https://github.com/flatpak/flatpak-builder-tools.git "$tools"

python3 "$tools/cargo/flatpak-cargo-generator.py" "$root/Cargo.lock" -o "$here/cargo-sources.json"
# Playwright only takes the screenshots for the README; its browsers are not needed to build.
PYTHONPATH="$tools/node" python3 -m flatpak_node_generator npm "$root/package-lock.json" \
  --no-playwright-browsers -o "$here/node-sources.json"
