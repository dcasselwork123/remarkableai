#!/bin/sh
# If an OS update is staged (u-boot active_partition names a partition we are
# not booted from), mount the new rootfs and install scribe's systemd units
# into it, so the daemon survives the A/B partition swap. Runs from a
# 5-minute timer and again at shutdown; idempotent, exits silently when no
# update is staged. Ported from riddle's xovi-persist (minus the xovi-specific
# hashtab/revert machinery — scribe is a static binary, nothing OS-version
# dependent to rebuild).
set -u

DIR=/home/root/scribe
LOG=$DIR/persist.log

booted=$(sed -n 's|.*root=/dev/mmcblk2p\([0-9]\).*|\1|p' /proc/cmdline)
active=$(/usr/sbin/fw_printenv active_partition 2>/dev/null | cut -d= -f2)
[ -n "$booted" ] && [ -n "$active" ] || exit 0
[ "$booted" != "$active" ] || exit 0

exec >>"$LOG" 2>&1
echo "== scribe-persist inject $(date): update staged on p$active (booted from p$booted) =="

MNT=/tmp/scribe-newroot
mkdir -p "$MNT"
if ! grep -q " $MNT " /proc/mounts; then
    mount "/dev/mmcblk2p$active" "$MNT" || { echo "mount failed"; exit 1; }
fi

if [ ! -d "$MNT/etc/systemd/system" ]; then
    echo "unexpected rootfs layout on p$active, aborting"
    umount "$MNT"
    exit 1
fi

# scribe itself, plus the persist units so the NEXT update is covered too.
cp "$DIR/scribe.service" "$MNT/etc/systemd/system/scribe.service"
for u in scribe-persist-inject.service scribe-persist-inject.timer \
         scribe-persist-shutdown.service; do
    cp "$DIR/persist/units/$u" "$MNT/etc/systemd/system/$u"
done
mkdir -p "$MNT/etc/systemd/system/multi-user.target.wants" \
         "$MNT/etc/systemd/system/timers.target.wants"
ln -sf /etc/systemd/system/scribe.service \
       "$MNT/etc/systemd/system/multi-user.target.wants/scribe.service"
ln -sf /etc/systemd/system/scribe-persist-shutdown.service \
       "$MNT/etc/systemd/system/multi-user.target.wants/scribe-persist-shutdown.service"
ln -sf /etc/systemd/system/scribe-persist-inject.timer \
       "$MNT/etc/systemd/system/timers.target.wants/scribe-persist-inject.timer"
umount "$MNT"
echo "scribe units injected into new rootfs on p$active"
