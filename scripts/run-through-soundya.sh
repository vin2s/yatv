#!/bin/sh
# Keeps YATV running: restarts it a few seconds after any exit.
# Installed through the immutable S99soundya init script: it runs this file
# from /data/opt/soundya/run.sh, so no system partition changes are needed.
DIR=/data/opt/yatv
echo $$ > /run/soundya.pid
# shellcheck disable=SC1091
[ -f "$DIR/yatv.env" ] && . "$DIR/yatv.env"
export YATV_DIR="$DIR"
while [ ! -f /run/soundya.stop ]; do
    "$DIR/yatv" daemon
    sleep 3
done
