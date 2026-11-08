#!/bin/bash
# Builds the Flatpak from this checkout and writes target/flatpak/lobby.flatpak, a bundle that
# `flatpak install --user lobby.flatpak` installs. Needs flatpak-builder, the runtimes the manifest
# names, and flatpak-cargo-generator from flatpak-builder-tools (FLATPAK_CARGO_GENERATOR, with
# Python that has aiohttp and tomlkit as FLATPAK_PYTHON).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
out="$root/target/flatpak"
mkdir -p "$out"
"${FLATPAK_PYTHON:-python3}" "${FLATPAK_CARGO_GENERATOR:?path to flatpak-cargo-generator.py}" \
  "$root/Cargo.lock" -o "$here/cargo-sources.json"
flatpak-builder --user --force-clean --install-deps-from=flathub --repo="$out/repo" \
  --state-dir="$out/state" "$out/build" "$here/io.github.frisksitron.Lobby.yml"
flatpak build-bundle "$out/repo" "$out/lobby.flatpak" io.github.frisksitron.Lobby
echo "$out/lobby.flatpak"
