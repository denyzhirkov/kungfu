#!/bin/sh
set -e

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)
case "$ARCH" in
    aarch64) ARCH="arm64" ;;
esac

BINARY_NAME="kungfu-${OS}-${ARCH}"

# Semantic (AI/vector) search is opt-in. It pulls in the candle ML stack (+~80MB binary,
# slower cold build) and needs a ~130MB model via `kungfu embeddings install`. Off by
# default to keep the distributed binary lean — enable explicitly with:
#   KUNGFU_SEMANTIC=1 ./build.sh
FEATURES=""
if [ "${KUNGFU_SEMANTIC:-0}" = "1" ]; then
    FEATURES="--features semantic"
    echo "Building WITH semantic (candle) support"
fi

cargo build --release ${FEATURES}
mkdir -p dist
cp target/release/kungfu "dist/${BINARY_NAME}"
cp target/release/kungfu dist/kungfu

echo "Built: dist/${BINARY_NAME}"
