#!/bin/sh
# YATV launcher: keeps the daemon running, writes its own pid and reads its env.
DIR=/data/opt/yatv
echo $$ > /run/yatv.pid
# shellcheck disable=SC1091
[ -f "$DIR/yatv.env" ] && . "$DIR/yatv.env"
export YATV_DIR="$DIR"
while [ ! -f /run/yatv.stop ]; do
    "$DIR/yatv" daemon
    sleep 3
done
