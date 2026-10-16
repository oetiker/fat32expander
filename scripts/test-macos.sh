#!/bin/bash
# Resize a FAT32 filesystem on a real macOS disk device.
#
# A raw disk image is attached with hdiutil, so the program sees /dev/diskN
# and /dev/rdiskN as it would see a USB stick. This exercises what unit tests
# on Linux cannot: the device size from the DKIOCGETBLOCK* ioctls (a seek to
# the end of a disk device gives 0 on macOS) and the mount check through
# getfsstat, including a raw device whose block twin is mounted.
#
# Needs sudo without a password, as on the GitHub macOS runners: the disk
# devices belong to root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BINARY="${1:-${CARGO_TARGET_DIR:-$PROJECT_DIR/target}/release/fat32expander}"
WORK_DIR="$(mktemp -d)"
IMAGE="$WORK_DIR/fat32.img"
DISK=""

cleanup() {
    if [ -n "$DISK" ]; then
        hdiutil detach "$DISK" -force >/dev/null 2>&1 || true
    fi
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

fail() {
    echo "FAILED: $1" >&2
    exit 1
}

# Attach the image without mounting it and print the disk device.
attach() {
    hdiutil attach -nomount -imagekey diskimage-class=CRawDiskImage "$IMAGE" \
        | awk 'NR == 1 { print $1 }'
}

detach() {
    hdiutil detach "$DISK" >/dev/null
    DISK=""
}

# Disk Arbitration may mount a volume as soon as it appears.
unmount() {
    diskutil unmount "$DISK" >/dev/null 2>&1 || true
}

# Run the program and print its exit code; never stop the script on it.
exit_code() {
    local code=0
    sudo "$BINARY" "$@" >"$WORK_DIR/out.txt" 2>&1 || code=$?
    echo "$code"
}

total_sectors() {
    sudo "$BINARY" info "$1" | awk '/Total sectors:/ { print $3; exit }'
}

[ -x "$BINARY" ] || fail "no binary at $BINARY"

echo "== Make a 64 MiB FAT32 image with files on it"
dd if=/dev/zero of="$IMAGE" bs=1m count=64 2>/dev/null
DISK="$(attach)"
echo "attached as $DISK"
sudo newfs_msdos -F 32 -v FAT32TEST "$DISK" >/dev/null
diskutil mount "$DISK" >/dev/null
MOUNT_POINT="$(diskutil info "$DISK" | awk -F': *' '/Mount Point/ { print $2 }')"
[ -d "$MOUNT_POINT" ] || fail "the new filesystem did not mount"
mkdir -p "$MOUNT_POINT/dir/sub"
head -c 300000 /dev/urandom >"$WORK_DIR/big.bin"
cp "$WORK_DIR/big.bin" "$MOUNT_POINT/dir/sub/big.bin"
echo "hello" >"$MOUNT_POINT/hello.txt"
unmount
detach

echo "== Grow the image to 128 MiB"
dd if=/dev/zero of="$IMAGE" bs=1m count=0 seek=128 2>/dev/null
DISK="$(attach)"
RAW_DISK="${DISK/\/dev\/disk//dev/rdisk}"
echo "attached as $DISK ($RAW_DISK)"

echo "== A mounted volume is refused, under its block and its raw name"
diskutil mount "$DISK" >/dev/null
code="$(exit_code resize "$DISK")"
[ "$code" = 4 ] || { cat "$WORK_DIR/out.txt"; fail "resize of mounted $DISK exited $code, not 4"; }
code="$(exit_code resize "$RAW_DISK")"
[ "$code" = 4 ] || { cat "$WORK_DIR/out.txt"; fail "resize of $RAW_DISK with $DISK mounted exited $code, not 4"; }
unmount

echo "== info sees the grown device"
before="$(total_sectors "$DISK")"
sudo "$BINARY" info "$DISK" | tee "$WORK_DIR/info.txt"
grep -q 'Can grow: Yes' "$WORK_DIR/info.txt" || fail "info does not see room to grow"

echo "== Resize through the raw device"
sudo "$BINARY" resize "$RAW_DISK"
unmount
after="$(total_sectors "$DISK")"
echo "total sectors: $before -> $after"
[ "$after" -gt "$before" ] || fail "the filesystem did not grow"

echo "== The result passes fsck_msdos and keeps its files"
sudo fsck_msdos -n "$DISK"
diskutil mount "$DISK" >/dev/null
MOUNT_POINT="$(diskutil info "$DISK" | awk -F': *' '/Mount Point/ { print $2 }')"
cmp "$WORK_DIR/big.bin" "$MOUNT_POINT/dir/sub/big.bin" || fail "dir/sub/big.bin changed"
[ "$(cat "$MOUNT_POINT/hello.txt")" = hello ] || fail "hello.txt changed"
unmount

echo "PASSED: resize on a macOS disk device"
