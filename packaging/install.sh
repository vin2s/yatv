#!/bin/sh
# YATV installer: works both for a fresh installation on a speaker and for an
# update (it is shipped inside the release archive and run by the updater).
# Files go to /data/opt/yatv; config.json is never overwritten.
set -u
SRC=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
DIR=/data/opt/yatv
LOG=/tmp/yatv-install.log
BAK="$DIR.pre-update"
ARCH=$(uname -m)
echo "$(date) installing yatv from $SRC ($ARCH)" >> "$LOG"

[ -x "$SRC/yatv" ] || { echo "no yatv binary in archive" >> "$LOG"; exit 1; }
# On a 64-bit station we must not install a 32-bit binary by mistake.
case "$ARCH" in
  aarch64) grep -q 'aarch64' "$SRC/VERSION" 2>/dev/null || true ;;
  armv7l)  : ;;
esac

# Keep one backup of the previous installation (updates only).
if [ -d "$DIR" ] && [ ! -d "$BAK" ]; then
    mkdir -p "$BAK" && cp -a "$DIR/." "$BAK/" 2>/dev/null
fi
mkdir -p "$DIR"

# Copy the program files. Never touch config.json / planner.json / journal.
for f in yatv run.sh yatv.env S99yatv VERSION install.sh; do
    [ -f "$SRC/$f" ] && cp "$SRC/$f" "$DIR/$f" && chmod 755 "$DIR/$f"
done
[ -f "$DIR/yatv.env" ] && chmod 644 "$DIR/yatv.env"

# Autostart: try the dedicated init script first; if /etc/init.d is read-only,
# hook into the existing immutable S99soundya or alice-homebrew start.sh.
installed=0
if [ -w /etc/init.d ] && [ -f "$DIR/S99yatv" ]; then
    cp "$DIR/S99yatv" /etc/init.d/S99yatv && chmod 755 /etc/init.d/S99yatv && installed=1
fi
if [ "$installed" = 0 ] && [ -x /etc/init.d/S99soundya ] && [ -w /data/opt/soundya/run.sh ]; then
    # This firmware's S99soundya is immutable and runs run.sh from /data.
    cp "$SRC/../run-through-soundya.sh" "$DIR/run-through-soundya.sh" 2>/dev/null || true
    cat > /data/opt/soundya/run.sh <<'EOF'
#!/bin/sh
DIR=/data/opt/yatv
echo $$ > /run/soundya.pid
[ -f "$DIR/yatv.env" ] && . "$DIR/yatv.env"
export YATV_DIR="$DIR"
while [ ! -f /run/soundya.stop ]; do
    "$DIR/yatv" daemon
    sleep 3
done
EOF
    chmod 755 /data/opt/soundya/run.sh && installed=1
fi
if [ "$installed" = 0 ] && [ -x /data/alice-homebrew/start.sh ] && [ -w /data/alice-homebrew/start.sh ]; then
    if ! grep -q YATV_AUTOSTART /data/alice-homebrew/start.sh; then
        cp /data/alice-homebrew/start.sh /data/alice-homebrew/start.sh.yatv-backup
        # Add exactly one hook after the Alice PID is recorded (BusyBox-safe).
        sed -i '/echo "$!" > "$PID"/a\    # YATV_AUTOSTART\n    if [ -x /data/opt/yatv/run.sh ]; then\n      nohup setsid sh /data/opt/yatv/run.sh </dev/null >>/data/opt/yatv/autostart.log 2>&1 &\n    fi' /data/alice-homebrew/start.sh
    fi
    installed=1
fi

echo "$(date) autostart installed=$installed" >> "$LOG"
killall yatv 2>/dev/null || true
if [ "$installed" = 1 ] && [ -x /etc/init.d/S99yatv ]; then
    /etc/init.d/S99yatv start >/dev/null 2>&1 &
elif [ "$installed" = 1 ] && [ -x /etc/init.d/S99soundya ]; then
    /etc/init.d/S99soundya start >/dev/null 2>&1 &
else
    nohup setsid sh "$DIR/run.sh" </dev/null >>"$DIR/autostart.log" 2>&1 &
fi
sleep 2
echo "$(date) done" >> "$LOG"
exit 0
