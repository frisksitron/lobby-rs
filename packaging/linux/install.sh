#!/bin/sh
# Installs Lobby for the current user on x86_64 Linux: downloads the newest Release's app, checks
# its SHA-256 against the Release's manifest and runs it. The app installs itself into
# ~/.local/share/lobby, adds its launcher entry and starts, and updates itself from then on.
#
#   sh install.sh
#
# LOBBY_RELEASE_BASE points it at another server laid out like GitHub Releases, for trying a
# release before it is published. Remove Lobby with `~/.local/share/lobby/lobby --uninstall`.

set -eu

BASE=${LOBBY_RELEASE_BASE:-https://github.com/frisksitron/lobby-rs/releases}
MANIFEST=latest-linux.json
ASSET=lobby-linux-x86_64

fail() {
    echo "install.sh: $*" >&2
    exit 1
}

[ "$(uname -s)" = Linux ] || fail "this installs Lobby on Linux"
[ "$(uname -m)" = x86_64 ] || fail "there is only an x86_64 build of Lobby for Linux"

if command -v curl >/dev/null 2>&1; then
    fetch() { curl --fail --silent --show-error --location --output "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget --quiet --output-document="$2" "$1"; }
else
    fail "curl or wget is needed to download Lobby"
fi
command -v sha256sum >/dev/null 2>&1 || fail "sha256sum is needed to check the download"

# Under the cache folder, not /tmp: the download is run from here, and some systems mount /tmp
# noexec.
cache=${XDG_CACHE_HOME:-$HOME/.cache}
mkdir -p "$cache" || fail "cannot make $cache"
work=$(mktemp -d "$cache/lobby-install.XXXXXX") || fail "cannot make a temporary folder"
trap 'rm -rf "$work"' EXIT HUP INT TERM

echo "Looking for the newest Lobby..."
fetch "$BASE/latest/download/$MANIFEST" "$work/manifest" || fail "cannot read $BASE/latest/download/$MANIFEST"

# One field of the manifest, a JSON object of strings and numbers.
field() {
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\{0,1\}\([^\",}]*\)\"\{0,1\}.*/\1/p" "$work/manifest" | head -n 1
}
version=$(field version)
size=$(field size)
sha256=$(field sha256)
case $version in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "the release manifest has no version" ;;
esac
case $size in
    '' | *[!0-9]*) fail "the release manifest has no size" ;;
esac
case $sha256 in
    *[!0-9a-f]*) fail "the release manifest has no SHA-256" ;;
esac
[ ${#sha256} -eq 64 ] || fail "the release manifest has no SHA-256"

echo "Downloading Lobby $version..."
fetch "$BASE/download/v$version/$ASSET" "$work/lobby" || fail "cannot download $BASE/download/v$version/$ASSET"

[ "$(wc -c <"$work/lobby" | tr -d ' ')" = "$size" ] || fail "the download is not the size the release says"
found=$(sha256sum "$work/lobby" | cut -d ' ' -f 1)
[ "$found" = "$sha256" ] || fail "the download does not match the release's SHA-256"

chmod 755 "$work/lobby"
echo "Installing..."
# Not exec: the temporary folder is removed once the app has installed itself and started.
"$work/lobby" "$@"
