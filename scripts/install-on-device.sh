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
sleep 1
systemctl --no-pager status scribe || true
echo
echo "scribe installed. Watch logs with:  journalctl -fu scribe"
echo "Stop with:  systemctl stop scribe   Remove autostart:  systemctl disable scribe"
