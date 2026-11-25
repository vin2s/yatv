#!/bin/sh
# YATV installer, run from an updated archive (in /tmp/yatv-update/yatv/).
# Copies files into place, keeps a backup, and exits. The running supervisor
# (run.sh / init hook) restarts the daemon with the new binary on its own.
set -u
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
DIR=/data/opt/yatv
LOG=/tmp/yatv-install.log
BAK="$DIR.pre-update"
echo "$(date) installing yatv from $SRC" >> "$LOG"

[ -x "$SRC/yatv" ] || { echo "no yatv binary in archive" >> "$LOG"; exit 1; }

# Keep one backup (do not overwrite a previous one).
if [ ! -d "$BAK" ]; then
    mkdir -p "$BAK" && cp -a "$DIR/." "$BAK/" 2>/dev/null
fi

for f in yatv run.sh yatv.env S99yatv VERSION install.sh; do
    [ -f "$SRC/$f" ] && cp "$SRC/$f" "$DIR/$f" 2>/dev/null && chmod 755 "$DIR/$f"
done
[ -f "$SRC/yatv.env" ] && chmod 644 "$DIR/yatv.env"

echo "$(date) files copied, restarting daemon" >> "$LOG"
# Kill only the daemon process; the supervisor restarts it. Do NOT call the
# init script here: it would start a second supervisor and race with this one.
killall yatv 2>/dev/null || true
exit 0
