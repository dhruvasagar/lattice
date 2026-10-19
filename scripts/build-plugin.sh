#!/usr/bin/env bash
# Build a lattice plugin and package it as a release asset.
#
#   scripts/build-plugin.sh <plugin-dir>                # build + check only
#   scripts/build-plugin.sh <plugin-dir> <out-dir>      # …and package
#   scripts/build-plugin.sh --no-build <plugin-dir> <out-dir>
#
# The one recipe a released plugin goes through (PRL.7,
# docs/dev/architecture/plugin-distribution.md §2, §8):
#
#   1. `cargo build --release --target wasm32-wasip2` in <plugin-dir>
#      (skipped with --no-build: package what is already built);
#   2. exactly one component in target/wasm32-wasip2/release, not empty;
#   3. it carries its manifest — the `lattice-manifest` custom section
#      (`lattice_plugin_sdk::manifest!`). A released plugin is one file or it
#      is not released;
#   4. with <out-dir>: write `<id>-<version>-api<M>.<m>.wasm` and its
#      `.sha256` there. `id` is the embedded manifest's, `version` the crate's,
#      and the API generation is the plugin's locked `lattice-wit`
#      (major.minor) — never hand-written, because it is what decides which
#      editors install the asset. A plugin that does not depend on
#      `lattice-wit` (the bundled ones) is built against this repository's,
#      so that version is used.
#
# Prints the asset's path on success. Needs cargo, the wasm32-wasip2 target
# and python3 (to read the custom section without a wasm toolchain).
set -euo pipefail
unset CDPATH

die() { printf 'build-plugin: %s\n' "$*" >&2; exit 1; }

build=1
if [ "${1:-}" = "--no-build" ]; then
    build=0
    shift
fi
[ "$#" -ge 1 ] && [ "$#" -le 2 ] || die "usage: build-plugin.sh [--no-build] <plugin-dir> [<out-dir>]"
plugin_dir="$(cd "$1" && pwd)" || die "no such directory: $1"
out_dir="${2:-}"
lattice_root="$(cd "$(dirname "$0")/.." && pwd)"

[ -f "$plugin_dir/Cargo.toml" ] || die "$plugin_dir is not a cargo project"

if [ "$build" -eq 1 ]; then
    (cd "$plugin_dir" && cargo build --release --target wasm32-wasip2) \
        || die "cargo build failed in $plugin_dir"
fi

shopt -s nullglob
components=("$plugin_dir"/target/wasm32-wasip2/release/*.wasm)
shopt -u nullglob
[ "${#components[@]}" -eq 1 ] \
    || die "expected exactly one component in target/wasm32-wasip2/release, found ${#components[@]}"
component="${components[0]}"
[ -s "$component" ] || die "$component is empty"

# The manifest's `id`, read from the component's `lattice-manifest` section —
# the same walk the editor's loader does: top-level sections, and the core
# modules a component wraps.
id="$(python3 - "$component" <<'PY'
import re, sys

def leb(b, i):
    r = s = 0
    while True:
        x = b[i]; i += 1
        r |= (x & 0x7F) << s
        s += 7
        if x < 0x80:
            return r, i

def manifest(b):
    if len(b) < 8 or b[:4] != b"\0asm":
        return None
    component = b[6] == 1 and b[7] == 0
    i = 8
    while i < len(b):
        sid = b[i]
        n, j = leb(b, i + 1)
        body = b[j:j + n]
        if sid == 0:
            ln, k = leb(body, 0)
            if body[k:k + ln] == b"lattice-manifest":
                return body[k + ln:]
        elif component and sid == 1:
            found = manifest(body)
            if found is not None:
                return found
        i = j + n
    return None

data = open(sys.argv[1], "rb").read()
text = manifest(data)
if text is None:
    sys.exit("no lattice-manifest section: embed the manifest with lattice_plugin_sdk::manifest!")
m = re.search(r'^\s*id\s*=\s*"([^"]+)"', text.decode("utf-8"), re.M)
if not m:
    sys.exit("the embedded manifest names no id")
print(m.group(1))
PY
)" || die "$component carries no usable manifest"

if [ -z "$out_dir" ]; then
    printf '%s\n' "$component"
    exit 0
fi

# The crate's version, and the API generation it was built against.
version="$(cd "$plugin_dir" && cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')" \
    || die "cannot read the crate version"
wit_version=""
if [ -f "$plugin_dir/Cargo.lock" ]; then
    wit_version="$(awk '/^name = "lattice-wit"$/ { getline; gsub(/version = |"/, ""); print; exit }' \
        "$plugin_dir/Cargo.lock")"
fi
if [ -z "$wit_version" ]; then
    wit_version="$(grep -m1 -E '^version[[:space:]]*=' "$lattice_root/crates/lattice-wit/Cargo.toml" \
        | sed -E 's/.*"([^"]+)".*/\1/')"
fi
api="$(printf '%s' "$wit_version" | cut -d. -f1-2)"
[ -n "$api" ] || die "cannot tell which plugin API generation this was built against"

mkdir -p "$out_dir"
asset="$id-$version-api$api.wasm"
cp "$component" "$out_dir/$asset"
(
    cd "$out_dir"
    if command -v sha256sum >/dev/null; then
        sha256sum "$asset" > "$asset.sha256"
    else
        shasum -a 256 "$asset" > "$asset.sha256"
    fi
)
printf '%s\n' "$out_dir/$asset"
