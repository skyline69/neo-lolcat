#!/usr/bin/env bash
set -euo pipefail

BIN="${BIN:-target/release/lolcat}"
BYTES="${BYTES:-1048576}"
CHUNK="${CHUNK:-65536}"

if [ ! -x "$BIN" ]; then
  echo "Build the release binary first (cargo build --release)" >&2
  exit 1
fi

echo "Streaming $BYTES bytes to $BIN (chunk size $CHUNK)..."
python3 - <<'PY'
import os, sys
total = int(os.environ.get("BYTES", 1048576))
chunk = int(os.environ.get("CHUNK", 65536))
data = os.urandom(chunk)
sent = 0
while sent < total:
    take = min(chunk, total - sent)
    os.write(sys.stdout.fileno(), data[:take])
    sent += take
PY
echo "Stress stream complete."
