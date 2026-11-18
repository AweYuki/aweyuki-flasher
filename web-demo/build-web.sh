#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

export PATH="$HOME/.cargo/bin:$PATH"

echo "[1/3] Building aweyuki-wasm for wasm32-unknown-unknown..."
cargo build --release --target wasm32-unknown-unknown --lib -p aweyuki-wasm

VER="$(awk '/^name = "wasm-bindgen"$/{getline; gsub(/"/, "", $3); print $3; exit}' Cargo.lock)"
echo "[2/3] Ensuring wasm-bindgen-cli v${VER}..."

NEED_DL=1
if command -v wasm-bindgen >/dev/null 2>&1; then
  CUR_VER="$(wasm-bindgen --version | awk '{print $2}')"
  if [ "$CUR_VER" = "$VER" ]; then
    NEED_DL=0
  fi
fi

if [ "$NEED_DL" -eq 1 ]; then
  TMP_DIR="$(mktemp -d)"
  URL="https://github.com/rustwasm/wasm-bindgen/releases/download/${VER}/wasm-bindgen-${VER}-x86_64-unknown-linux-musl.tar.gz"
  echo "Downloading ${URL}..."
  curl -fsSL "$URL" -o "$TMP_DIR/wbg.tar.gz"
  tar -xzf "$TMP_DIR/wbg.tar.gz" -C "$TMP_DIR"
  mkdir -p "$HOME/.cargo/bin"
  cp "$TMP_DIR"/wasm-bindgen-*/wasm-bindgen "$HOME/.cargo/bin/wasm-bindgen"
  chmod +x "$HOME/.cargo/bin/wasm-bindgen"
  rm -rf "$TMP_DIR"
fi

echo "[3/3] Generating web-demo/pkg JS+WASM bindings..."
rm -rf web-demo/pkg
mkdir -p web-demo/pkg
wasm-bindgen target/wasm32-unknown-unknown/release/aweyuki_wasm.wasm \
  --out-dir web-demo/pkg \
  --target web \
  --no-typescript

ls -lh web-demo/pkg/
echo "Done! Serve with: python3 -m http.server 8787 --directory web-demo"
