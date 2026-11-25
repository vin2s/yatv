#!/bin/sh
# YATV installer, run from an updated archive (in /tmp/yatv-update/yatv/).
# Copies files into place, keeps a backup, restarts the daemon in its own
# session, and rolls back if the new binary does not come up.
set -u
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
DIR=/data/opt/yatv
LOG=/tmp/yatv-install.log
BAK="$DIR.pre-update"
echo "$(date) installing yatv from $SRC" >> "$LOG"

[ -x "$SRC/yatv" ] || { echo "no yatv binary in archive" >> "$LOG"; exit 1; }

# Backup the current installation once (do not overwrite a previous backup).
if [ ! -d "$BAK" ]; then
    mkdir -p "$BAK" && cp -a "$DIR/." "$BAK/" 2>/dev/null
fi

# The archive places files in yatv/; copy only known files, keep config.json.
for f in yatv run.sh yatv.env S99yatv VERSION; do
    [ -f "$SRC/$f" ] && cp "$SRC/$f" "$DIR/$f" && chmod 755 "$DIR/$f"
done
[ -f "$DIR/yatv.env" ] && chmod 644 "$DIR/yatv.env"

# Restart through whichever init hook this speaker uses.
start_yatv() {
    if [ -x /etc/init.d/S99yatv ]; then /etc/init.d/S99yatv restart >/dev/null 2>&1; return; fi
    if [ -x /etc/init.d/S99soundya ]; then /etc/init.d/S99soundya restart >/dev/null 2>&1; return; fi
    if [ -x /data/alice-homebrew/start.sh ]; then /data/alice-homebrew/start.sh start >/dev/null 2>&1; fi
}
stop_yatv() {
    killall yatv 2>/dev/null || true
    [ -x /etc/init.d/S99yatv ] && /etc/init.d/S99yatv stop >/dev/null 2>&1
    [ -x /etc/init.d/S99soundya ] && /etc/init.d/S99soundya stop >/dev/null 2>&1
}

stop_yatv
sleep 1
start_yatv
sleep 4

if pgrep yatv >/dev/null 2>&1; then
    echo "$(date) update ok: $(cat "$DIR/VERSION" 2>/dev/null)" >> "$LOG"
    rm -rf "$BAK"
else
    echo "$(date) new binary did not start — rolling back" >> "$LOG"
    stop_yatv
    [ -d "$BAK" ] && cp -a "$BAK/." "$DIR/" 2>/dev/null
    sleep 1
    start_yatv
    echo "$(date) rolled back" >> "$LOG"
fi
