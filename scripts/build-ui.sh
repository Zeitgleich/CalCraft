#!/bin/sh
# Builds the web UI (crates/calcraft-ui) into crates/calcraft-ui/dist/.
# The next build of calcraft-server embeds these files; without them the
# server shows a placeholder page instead.
#
# Requires the wasm32-unknown-unknown target (see rust-toolchain.toml) and
# the wasm-bindgen CLI in exactly the version pinned in
# crates/calcraft-ui/Cargo.toml:
#   cargo install wasm-bindgen-cli --version <version> --locked
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
ui="$root/crates/calcraft-ui"
dist="$ui/dist"
target_dir=${CARGO_TARGET_DIR:-"$root/target"}
wasm_bindgen=${WASM_BINDGEN:-wasm-bindgen}

pinned=$(sed -n 's/^wasm-bindgen = "=\([0-9.]*\)".*/\1/p' "$ui/Cargo.toml")
installed=$("$wasm_bindgen" --version | awk '{print $2}')
if [ "$pinned" != "$installed" ]; then
    echo "error: wasm-bindgen CLI $installed does not match the pinned crate version $pinned" >&2
    echo "       cargo install wasm-bindgen-cli --version $pinned --locked" >&2
    exit 1
fi

cargo build --manifest-path "$root/Cargo.toml" --locked -p calcraft-ui \
    --target wasm32-unknown-unknown --profile wasm-release

rm -rf "$dist"
mkdir -p "$dist"
"$wasm_bindgen" --target web --no-typescript --out-dir "$dist" --out-name app \
    "$target_dir/wasm32-unknown-unknown/wasm-release/calcraft-ui.wasm"
cp "$ui/style.css" "$dist/style.css"

echo "web UI assets written to $dist:"
ls -l "$dist"
