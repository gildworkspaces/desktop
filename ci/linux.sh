#!/usr/bin/env bash
# One CI job inside a fresh Debian container (node:24-bookworm), so the
# host runner installs nothing. Usage: ci/linux.sh web|native|app|shots
set -euo pipefail
job=${1:?web, native, app or shots}
export DEBIAN_FRONTEND=noninteractive CARGO_TERM_COLOR=never
apt() { apt-get update -qq && apt-get install -y -qq --no-install-recommends "$@" > /dev/null; }
rust() {
  curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --component clippy > /dev/null
  . "$HOME/.cargo/env"
  # gild's own add-ons are WebAssembly modules, built with the app.
  rustup target add wasm32-unknown-unknown > /dev/null
}
tauri_deps="build-essential pkg-config libssl-dev libwebkit2gtk-4.1-dev libxdo-dev libayatana-appindicator3-dev librsvg2-dev"
case $job in
  web)
    npm ci
    npm run check
    npm run build
    ;;
  native)
    apt $tauri_deps
    rust
    npm ci
    npm run build
    cargo clippy --locked --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings
    cargo clippy --locked --target wasm32-unknown-unknown --manifest-path src-tauri/addons/markdown/Cargo.toml -- -D warnings
    cargo test --locked --lib --manifest-path src-tauri/Cargo.toml
    ;;
  app)
    apt $tauri_deps webkit2gtk-driver xvfb xauth
    rust
    npm ci
    cargo install tauri-driver --locked -q
    npx tauri build --debug --no-bundle
    xvfb-run -a sh -c 'tauri-driver & sleep 3; node tests/e2e/smoke.mjs "$PWD/src-tauri/target/debug/gild-desktop"'
    ;;
  shots)
    # The screenshots on gild.addis.gg, from the real app (scripts/shots.mjs).
    apt $tauri_deps webkit2gtk-driver xvfb xauth
    rust
    npm ci
    command -v tauri-driver > /dev/null || cargo install tauri-driver --locked -q
    npx tauri build --debug --no-bundle
    xvfb-run -a -s '-screen 0 1600x1000x24' sh -c 'tauri-driver & sleep 3; node scripts/shots.mjs "$PWD/src-tauri/target/debug/gild-desktop" shots'
    ;;
esac
echo "ci/linux.sh $job: ok"
