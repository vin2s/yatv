#!/bin/sh
# Cross-builds YATV for the Yandex Station (ARMv7 hard-float, glibc <= 2.23).
# Runs in WSL / Linux. Does not touch yatv.
set -eu
ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
CACHE=${YATV_CACHE:-"${XDG_CACHE_HOME:-$HOME/.cache}/yatv"}
TC_NAME=gcc-linaro-6.5.0-2018.12-x86_64_arm-linux-gnueabihf
TC="$CACHE/$TC_NAME"
TARGET=armv7-unknown-linux-gnueabihf
mkdir -p "$CACHE"
if [ ! -x "$TC/bin/arm-linux-gnueabihf-gcc" ]; then
    archive="$CACHE/$TC_NAME.tar.xz"
    if [ ! -f "$archive" ]; then
        curl --fail --location --retry 3 --output "$archive.part" \
            "https://developer.arm.com/-/cdn-downloads/permalink/legacy-linaro-gnu-toolchains/6.5-2018.12/$TC_NAME.tar.xz"
        mv "$archive.part" "$archive"
    fi
    echo "a84b1239765617f3192014cb259c05e32771a64a4c831cf89577fcd87918eacd  $archive" | sha256sum -c -
    mkdir -p "$TC"
    tar -xJf "$archive" --strip-components=1 -C "$TC"
fi
rustup target add "$TARGET" >/dev/null
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER="$TC/bin/arm-linux-gnueabihf-gcc"
export CC_armv7_unknown_linux_gnueabihf="$TC/bin/arm-linux-gnueabihf-gcc"
export AR_armv7_unknown_linux_gnueabihf="$TC/bin/arm-linux-gnueabihf-ar"

cd "$ROOT"
cargo build --release --target "$TARGET"
BIN="target/$TARGET/release/yatv"
"$TC/bin/arm-linux-gnueabihf-readelf" --version-info "$BIN" \
    | grep -o 'GLIBC_[0-9.]*' | sort -u -V | tail -1 > /dev/null

PKG="$ROOT/dist/yatv"
rm -rf "$PKG" && mkdir -p "$PKG"
cp "$BIN" "$PKG/yatv"
cp packaging/run.sh packaging/yatv.env packaging/S99yatv "$PKG/"
# A checkout on Windows has CRLF: the speaker's /bin/sh cannot run such a script.
for f in run.sh yatv.env S99yatv; do
    tr -d '\r' < "$PKG/$f" > "$PKG/$f.new" && mv "$PKG/$f.new" "$PKG/$f"
done
chmod 755 "$PKG/yatv" "$PKG/run.sh" "$PKG/S99yatv"
sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1 | tr -d '\r' > "$PKG/VERSION"
(cd "$PKG" && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort \
    | while read -r f; do sha256sum "$f"; done > SHA256SUMS)
COPYFILE_DISABLE=1 tar -C "$ROOT/dist" -cf "$ROOT/dist/yatv-armv7.tar" yatv
echo "Built dist/yatv-armv7.tar"
