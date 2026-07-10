#!/bin/sh
# Assemble the static web build into web/dist — the exact tree GitHub Pages
# deploys, servable by any static file server.
# Fully static: .wasm + .js + .html + the packed data. No server backend.
set -eu
cd "$(dirname "$0")/.."

# always regenerate: stale packs from an older format are worse than the
# few seconds this takes. PRET overrides the pret clone location (CI).
PRET="${PRET:-$HOME/pokeemerald}"
cargo run -p worldgen --release -- "$PRET"
cargo run -p musicgen --release -- "$PRET"

cargo build --profile web --target wasm32-unknown-unknown -p backends --bin emerald

rm -rf web/dist
mkdir -p web/dist/assets
cp target/wasm32-unknown-unknown/web/emerald.wasm web/dist/
cp web/index.html web/gl.js web/dist/
cp assets/world.bin web/dist/assets/
# one pack per song: the page streams a song only when a map asks for it
# (GitHub Pages gzips .bin in transit; no pre-compression step needed)
cp -r assets/music web/dist/assets/

du -sh web/dist
echo "web/dist ready — served at /emerald by the dashboard front door"
