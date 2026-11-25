#!/bin/sh
# Cross-builds YATV for the 64-bit Yandex Station (aarch64, static musl: no
# glibc-version problems on the speaker). Runs in WSL / Linux.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-musl
CC=aarch64-linux-gnu-gcc
AR=aarch64-linux-gnu-ar
command -v "$CC" >/dev/null || { echo "no $CC: install gcc-aarch64-linux-gnu"; exit 1; }
rustup target add "$TARGET" >/dev/null
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$CC"
export CC_aarch64_unknown_linux_musl="$CC"
export AR_aarch64_unknown_linux_musl="$AR"
cd "$ROOT"
cargo build --release --target "$TARGET"
BIN="target/$TARGET/release/yatv"
if aarch64-linux-gnu-readelf -l "$BIN" | grep -q interpreter; then
    echo "not static: musl linker did not work"; exit 1
fi
# The archive root must be `yatv/` so install.sh is at a fixed path.
PKG="$ROOT/dist/yatv-aarch64"
rm -rf "$PKG" "$ROOT/dist/staging-aarch64" && mkdir -p "$ROOT/dist/staging-aarch64/yatv"
STAGE="$ROOT/dist/staging-aarch64/yatv"
cp "$BIN" "$STAGE/yatv"
cp packaging/run.sh packaging/yatv.env packaging/S99yatv packaging/install.sh "$STAGE/"
for f in run.sh yatv.env S99yatv install.sh; do
    tr -d '\r' < "$STAGE/$f" > "$STAGE/$f.new" && mv "$STAGE/$f.new" "$STAGE/$f"
done
chmod 755 "$STAGE/yatv" "$STAGE/run.sh" "$STAGE/S99yatv" "$STAGE/install.sh"
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1 | tr -d '\r' > "$STAGE/VERSION"
(cd "$STAGE" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort \
    | while read -r f; do sha256sum "$f"; done > SHA256SUMS)
COPYFILE_DISABLE=1 tar -C "$ROOT/dist/staging-aarch64" -cf "$ROOT/dist/yatv-aarch64.tar" yatv
echo "Built dist/yatv-aarch64.tar"
