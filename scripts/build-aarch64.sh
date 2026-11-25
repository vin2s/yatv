#!/bin/sh
# Cross-builds YATV for the 64-bit Yandex Station (aarch64, glibc <= 2.18).
# Runs in WSL / Linux. Does not touch the speaker.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
CC=aarch64-linux-gnu-gcc
AR=aarch64-linux-gnu-ar
command -v "$CC" >/dev/null || { echo "no $CC: install gcc-aarch64-linux-gnu"; exit 1; }
rustup target add "$TARGET" >/dev/null
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$CC"
export CC_aarch64_unknown_linux_gnu="$CC"
export AR_aarch64_unknown_linux_gnu="$AR"
cd "$ROOT"
cargo build --release --target "$TARGET"
BIN="target/$TARGET/release/yatv"
MAX_GLIBC=$(aarch64-linux-gnu-readelf --version-info "$BIN" | grep -o 'GLIBC_[0-9.]*' | sort -V | tail -1)
echo "max glibc: $MAX_GLIBC (station has 2.18)"
case "$MAX_GLIBC" in
  GLIBC_2.1[89]|GLIBC_2.[2-9]*) echo "too new for the station"; exit 1 ;;
esac
PKG="$ROOT/dist/yatv-aarch64"
rm -rf "$PKG" && mkdir -p "$PKG"
cp "$BIN" "$PKG/yatv"
cp packaging/run.sh packaging/yatv.env packaging/S99yatv "$PKG/"
for f in run.sh yatv.env S99yatv; do
    tr -d '\r' < "$PKG/$f" > "$PKG/$f.new" && mv "$PKG/$f.new" "$PKG/$f"
done
chmod 755 "$PKG/yatv" "$PKG/run.sh" "$PKG/S99yatv"
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1 | tr -d '\r' > "$PKG/VERSION"
(cd "$PKG" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort \
    | while read -r f; do sha256sum "$f"; done > SHA256SUMS)
COPYFILE_DISABLE=1 tar -C "$ROOT/dist" -cf "$ROOT/dist/yatv-aarch64.tar" yatv-aarch64
echo "Built dist/yatv-aarch64.tar"
