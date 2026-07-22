#!/bin/sh
# Stage the deployable bundle in dist/scribe/.
# Build first:
#   docker run --rm -v "$PWD":/home/rust/src messense/rust-musl-cross:armv7-musleabihf \
#       cargo build --release
set -e
cd "$(dirname "$0")/.."

BIN=target/armv7-unknown-linux-musleabihf/release/scribe
if [ ! -f "$BIN" ]; then
    echo "error: no armv7 scribe binary found — build it first (see header)" >&2
    exit 1
fi

rm -rf dist/scribe
mkdir -p dist/scribe
install -m 755 "$BIN" dist/scribe/scribe
install -m 644 oracle.env.example scribe.service dist/scribe/
install -m 755 scripts/install-on-device.sh dist/scribe/
# Alternate hand for SCRIBE_FONT (Patrick Hand is built in).
install -m 644 fonts/DancingScript.ttf dist/scribe/

echo "staged dist/scribe/"
echo "  1. cp your grok auth:   cp /path/to/riddle-auth.json dist/scribe/scribe-auth.json"
echo "     (or: cp oracle.env.example dist/scribe/oracle.env and put an API key in it)"
echo "  2. scp -O -r dist/scribe root@10.11.99.1:/home/root/scribe"
echo "  3. ssh root@10.11.99.1 '/home/root/scribe/install-on-device.sh'"
