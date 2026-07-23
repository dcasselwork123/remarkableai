#!/bin/sh
# Run ON the tablet after copying the bundle to /home/root/scribe.
# Installs + starts the systemd service. Safe to re-run (e.g. after an OS
# update wipes /etc/systemd — /home/root survives updates, the unit file
# does not).
set -e
DIR=/home/root/scribe

chmod +x "$DIR/scribe"
cp "$DIR/scribe.service" /etc/systemd/system/scribe.service
systemctl daemon-reload
systemctl enable scribe
systemctl restart scribe

# Arm update persistence: a staged A/B OS update gets the units injected into
# its rootfs before the swap (5-min timer + shutdown hook), so an update no
# longer silently kills scribe.
if [ -d "$DIR/persist/units" ]; then
    chmod +x "$DIR/persist/check-update.sh"
    for u in scribe-persist-inject.service scribe-persist-inject.timer \
             scribe-persist-shutdown.service; do
        cp "$DIR/persist/units/$u" "/etc/systemd/system/$u"
    done
    systemctl daemon-reload
    systemctl enable scribe-persist-inject.timer scribe-persist-shutdown.service
    systemctl start scribe-persist-inject.timer scribe-persist-shutdown.service
    echo "scribe-persist armed: OS updates will carry scribe across automatically"
fi

sleep 1
systemctl --no-pager status scribe || true
echo
echo "scribe installed. Watch logs with:  journalctl -fu scribe"
echo "Stop with:  systemctl stop scribe   Remove autostart:  systemctl disable scribe"
