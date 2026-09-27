#!/bin/sh
# Builds the web demo into www/pkg: the wasm32 target through the
# workspace's [profile.web], wasm-bindgen for the JS glue, then wasm-opt
# when it's installed (binaryen). Needs `rustup target add
# wasm32-unknown-unknown` and wasm-bindgen-cli at the version Cargo.lock
# pins for wasm-bindgen. Serve www/ over HTTP to run it, e.g.
#   python3 -m http.server -d hosts/web/www 8028
set -eu
cd "$(dirname "$0")"
root=$(cd ../.. && pwd)
cargo build -p raam-web --target wasm32-unknown-unknown --profile web
want=$(sed -n '/^name = "wasm-bindgen"$/{n;s/version = "\(.*\)"/\1/p;}' "$root/Cargo.lock")
have=$(wasm-bindgen --version | cut -d' ' -f2)
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen-cli is $have; Cargo.lock wants $want (cargo install wasm-bindgen-cli --version $want)" >&2
  exit 1
fi
wasm-bindgen --target web --no-typescript --out-dir www/pkg \
  "$root/target/wasm32-unknown-unknown/web/raam_web.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -Os --enable-bulk-memory --enable-nontrapping-float-to-int \
    -o www/pkg/raam_web_bg.wasm www/pkg/raam_web_bg.wasm
else
  echo "wasm-opt not found (binaryen): the wasm is unoptimised" >&2
fi
ls -l www/pkg
