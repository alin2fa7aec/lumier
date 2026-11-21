#!/usr/bin/env bash
# Starts the API server and the Vite dev server together, for checking the app
# in a browser at http://localhost:5173. Ctrl-C stops both.
#
#   bash tools/dev.sh
#
# Defaults to the generated fixtures, creating them on the first run. Point it
# at another library with LUMIER_ROOT and LUMIER_MANIFEST (absolute, or relative
# to the repository root).
#
#   bash tools/dev.sh --lan
#
# also serves Vite on the LAN, for checking from a phone. Only Vite's port 5173
# is exposed: lumier itself stays on 127.0.0.1 and the phone reaches it through
# Vite's proxy. There is no authentication, so anyone on the network can browse
# the library while it runs, and the host's firewall must let 5173 in.

set -euo pipefail

vite_args=(--strictPort)
case ${1:-} in
  '') ;;
  --lan) vite_args+=(--host) ;;
  *)
    echo "usage: bash tools/dev.sh [--lan]" >&2
    exit 2
    ;;
esac

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"

root=${LUMIER_ROOT:-sample-photos}
manifest=${LUMIER_MANIFEST:-museum.sample.yaml}
case $manifest in /*) ;; *) manifest="$repo/$manifest" ;; esac

if [[ $root == sample-photos && ! -d sample-photos/hall ]]; then
  for d in hall north south east; do
    node tools/make-sample-images.mjs "sample-photos/$d" 12 >/dev/null
  done
  echo "generated fixtures in sample-photos/"
fi

if [[ ! -d web/node_modules ]]; then
  npm ci --prefix web
fi

# Build first so compile time does not count against the readiness wait below.
cargo build --bin lumier
./target/debug/lumier serve --root "$root" --manifest "$manifest" &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT

for _ in $(seq 100); do
  if curl -fs -o /dev/null http://127.0.0.1:8080/api/museum; then break; fi
  if ! kill -0 "$server" 2>/dev/null; then
    echo "lumier exited before it started listening" >&2
    exit 1
  fi
  sleep 0.1
done

cd web
npm run dev -- "${vite_args[@]}"
