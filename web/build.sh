#!/usr/bin/env bash
# Builds the browser version into web/dist: a static folder for any HTTPS host (GitHub Pages).
# Needs the wasm32-unknown-unknown target, wasm-bindgen-cli at the version in Cargo.lock, and
# WASI_SDK_PATH pointing at wasi-sdk 34 (oxijolt compiles Jolt for the browser with it).
# Adapted from Rune's scripts/web-build.sh (MIT).
set -euo pipefail
cd "$(dirname "$0")/.."
out=web/dist
: "${WASI_SDK_PATH:?set WASI_SDK_PATH to an unpacked wasi-sdk 34}"

# small and fast: the defaults Rune's web build uses
export CARGO_PROFILE_RELEASE_OPT_LEVEL=${CARGO_PROFILE_RELEASE_OPT_LEVEL:-3} \
    CARGO_PROFILE_RELEASE_LTO=${CARGO_PROFILE_RELEASE_LTO:-thin} CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_DEBUG=0 CARGO_PROFILE_RELEASE_STRIP=debuginfo
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=/home --remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}=/cargo"
cargo build --release --locked --target wasm32-unknown-unknown --bin mines-afloat

want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"/\1/p')
if [[ $(wasm-bindgen --version 2>/dev/null) != "wasm-bindgen $want" ]]; then
    echo "install the matching CLI: cargo install wasm-bindgen-cli --version $want --locked" >&2
    exit 2
fi
rm -rf "$out" && mkdir -p "$out"
wasm-bindgen --target web --out-dir "$out" --out-name app --remove-name-section \
    target/wasm32-unknown-unknown/release/mines-afloat.wasm

# Rune embeds Inter cut to a few scripts and fetches the full faces from beside app.js
py=$(command -v python3 || command -v python)
rune_dir=$(cargo metadata --format-version 1 --locked | "$py" -c '
import json, os, sys
m = json.load(sys.stdin)
print(os.path.dirname(next(p["manifest_path"] for p in m["packages"] if p["name"] == "rune")))')
cp "$rune_dir"/assets/web/Inter-*.ttf "$out/"
cp web/index.html "$out/"
cp docs/gameplay.gif "$out/" 2>/dev/null || true
echo "built $out"
