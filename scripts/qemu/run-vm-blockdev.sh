#!/bin/bash
#
# Run the block device resize test inside a QEMU VM
#
# Every other scenario resizes an image file. This one resizes a partition
# of a loop device, the case the tool exists for ([F-T07]):
#   1. A disk image with an MBR and one 128 MiB partition, attached with
#      `losetup -P`, so the filesystem lives on /dev/loop0p1
#   2. Resize while the partition is mounted: exit 4, "currently mounted"
#   3. Resize while it is mounted under another device node, which the
#      mount table check cannot match: the kernel refuses the exclusive
#      open, exit 4, "is in use"
#   4. Grow the disk and the partition to 256 MiB, re-read the partition
#      table, resize /dev/loop0p1 (sized by BLKGETSIZE64), then fsck, check
#      the new size and compare checksums
#

set -e

WORK_DIR="$1"
BINARY="$2"
KERNEL="$3"
INITRD="$4"
VM_TIMEOUT="${5:-300}"

if [ -z "$WORK_DIR" ] || [ -z "$BINARY" ]; then
    echo "Usage: $0 <work_dir> <binary> <kernel> <initrd> [timeout]"
    exit 1
fi

SERIAL_LOG="$WORK_DIR/vm-console.log"
OVERLAY_CPIO="$WORK_DIR/overlay.cpio"
COMBINED_INITRD="$WORK_DIR/combined-initrd"

# The partition starts at sector 2048. The disk is 1 MiB larger than the
# partition, so 129 MiB holds a 128 MiB partition and 257 MiB a 256 MiB one.
PART_START=2048
SMALL_SECTORS=262144
LARGE_SECTORS=524288

# Download Alpine package if not cached
download_alpine_pkg() {
    local pkg="$1"
    local cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/fat32expander-test/apk"
    local pkg_file="$cache_dir/$pkg.apk"

    mkdir -p "$cache_dir"

    if [ ! -f "$pkg_file" ]; then
        echo "Downloading Alpine package: $pkg..." >&2
        local url="https://dl-cdn.alpinelinux.org/alpine/v3.20/main/x86_64"
        curl -sL "$url/APKINDEX.tar.gz" | tar -xzO APKINDEX | \
            grep -A1 "^P:$pkg$" | grep "^V:" | head -1 | cut -d: -f2 > "$cache_dir/$pkg.version"
        local version=$(cat "$cache_dir/$pkg.version")
        curl -sL -o "$pkg_file" "$url/${pkg}-${version}.apk"
    fi

    echo "$pkg_file"
}

# Extract binaries from Alpine package
extract_alpine_pkg() {
    local pkg_file="$1"
    local dest_dir="$2"
    tar -xzf "$pkg_file" -C "$dest_dir" --warning=no-unknown-keyword 2>&1 | grep -v "Ignoring unknown" || true
}

# A 32-bit value as four little-endian printf escapes
le32() {
    local v=$1
    printf '\\x%02x\\x%02x\\x%02x\\x%02x' \
        $((v & 255)) $(((v >> 8) & 255)) $(((v >> 16) & 255)) $(((v >> 24) & 255))
}

# An MBR with one FAT32 (LBA) partition of $1 sectors at $PART_START
write_mbr() {
    local sectors=$1 out=$2
    {
        head -c 446 /dev/zero
        printf '\x00\x00\x00\x00\x0c\x00\x00\x00'
        printf "$(le32 $PART_START)$(le32 $sectors)"
        head -c 48 /dev/zero
        printf '\x55\xaa'
    } > "$out"
    [ "$(stat -c %s "$out")" = 512 ] || { echo "ERROR: MBR is not 512 bytes"; exit 1; }
}

# Create the cpio overlay
create_overlay() {
    local overlay_dir="$WORK_DIR/overlay"
    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir"/{bin,sbin,usr/sbin,scripts}

    cp "$BINARY" "$overlay_dir/bin/fat32expander"
    chmod +x "$overlay_dir/bin/fat32expander"

    echo "Adding dosfstools to overlay..."
    local dosfstools_pkg=$(download_alpine_pkg "dosfstools")
    extract_alpine_pkg "$dosfstools_pkg" "$overlay_dir"

    # busybox losetup has no -P (partition scan)
    echo "Adding losetup to overlay..."
    local libsmartcols_pkg=$(download_alpine_pkg "libsmartcols")
    extract_alpine_pkg "$libsmartcols_pkg" "$overlay_dir"
    local losetup_pkg=$(download_alpine_pkg "losetup")
    extract_alpine_pkg "$losetup_pkg" "$overlay_dir"
    chmod +x "$overlay_dir/sbin/"* 2>/dev/null || true
    if [ ! -f "$overlay_dir/sbin/losetup" ]; then
        echo "ERROR: Failed to extract losetup"
        exit 1
    fi

    write_mbr $SMALL_SECTORS "$overlay_dir/scripts/mbr-small.bin"
    write_mbr $LARGE_SECTORS "$overlay_dir/scripts/mbr-large.bin"

    cat > "$overlay_dir/init" << 'INIT_SCRIPT'
#!/bin/busybox sh
# Block device resize test init

/bin/busybox mkdir -p /proc /sys /dev /tmp /mnt
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t tmpfs tmpfs /tmp

exec > /dev/ttyS0 2>&1

echo "=== Block Device Resize Test Starting ==="

/bin/busybox --install -s /bin 2>/dev/null || true
[ -e /dev/loop0 ] || mknod /dev/loop0 b 7 0

DISK="/tmp/disk.img"
PART="/dev/loop0p1"
MOUNT_POINT="/mnt/fat"
mkdir -p "$MOUNT_POINT"

fail() {
    echo "GUEST_ERROR: $*"
    echo o > /proc/sysrq-trigger
    sleep 999
}

# Attach the disk with a partition scan and wait for the partition node
attach() {
    /sbin/losetup -P /dev/loop0 "$DISK" || fail "losetup -P failed"
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ -b "$PART" ] && return 0
        sleep 1
    done
    fail "$PART did not appear"
}

# Run a resize that must be refused with exit 4 and a message containing $1
expect_refusal() {
    local expected="$1"
    /bin/fat32expander resize "$PART" > /tmp/refusal.out 2>&1
    local rc=$?
    cat /tmp/refusal.out
    [ $rc -eq 4 ] || fail "resize of a $2 partition exited $rc, expected 4"
    grep -q "$expected" /tmp/refusal.out || fail "refusal did not say \"$expected\""
}

echo ""
echo "=== Phase 1: 129 MiB disk with one 128 MiB partition ==="
dd if=/dev/zero of="$DISK" bs=1M count=129 2>/dev/null
dd if=/scripts/mbr-small.bin of="$DISK" conv=notrunc 2>/dev/null
attach
echo "Partition size: $(cat /sys/class/block/loop0p1/size) sectors"
/sbin/mkfs.fat -F 32 "$PART" >/dev/null || fail "mkfs.fat failed"

mount -t vfat "$PART" "$MOUNT_POINT" || fail "mount failed"
cd "$MOUNT_POINT"
echo "First file" > file1.txt
dd if=/dev/urandom of=data1.bin bs=1024 count=300 2>/dev/null
mkdir -p sub/deeper
echo "Nested file" > sub/deeper/nested.txt
dd if=/dev/urandom of=sub/data2.bin bs=1024 count=700 2>/dev/null
find . -type f -exec sha256sum {} \; | sort > /tmp/before.sha
cat /tmp/before.sha
cd /

echo ""
echo "=== Phase 2: resize while mounted ==="
expect_refusal "currently mounted" "mounted"
echo "BLOCKDEV_MOUNTED_REFUSED"
umount "$MOUNT_POINT" || fail "umount failed"

echo ""
echo "=== Phase 3: resize while mounted under another device node ==="
mknod /tmp/alias b $(tr ':' ' ' < /sys/class/block/loop0p1/dev) || fail "mknod failed"
mount -t vfat /tmp/alias "$MOUNT_POINT" || fail "mount through the alias failed"
grep "^/tmp/alias " /proc/mounts || fail "the mount table does not name the alias"
expect_refusal "is in use" "busy"
echo "BLOCKDEV_BUSY_REFUSED"
umount "$MOUNT_POINT" || fail "umount failed"

/sbin/fsck.fat -n "$PART" || fail "fsck.fat after the refusals failed"

echo ""
echo "=== Phase 4: grow the disk and the partition to 256 MiB ==="
/sbin/losetup -d /dev/loop0 || fail "losetup -d failed"
dd if=/dev/zero bs=1M count=128 >> "$DISK" 2>/dev/null
dd if=/scripts/mbr-large.bin of="$DISK" conv=notrunc 2>/dev/null
attach
SIZE=$(cat /sys/class/block/loop0p1/size)
echo "Partition size: $SIZE sectors"
[ "$SIZE" = 524288 ] || fail "the partition did not grow: $SIZE sectors"

/bin/fat32expander resize --verbose "$PART"
RC=$?
[ $RC -eq 0 ] || fail "resize of $PART exited $RC"

/sbin/fsck.fat -n "$PART" || fail "fsck.fat after the resize failed"

/bin/fat32expander info "$PART" > /tmp/info.out 2>&1 || fail "info failed"
cat /tmp/info.out
grep -q "Total sectors: 524288" /tmp/info.out || fail "the filesystem does not fill the partition"

mount -t vfat "$PART" "$MOUNT_POINT" || fail "mount after the resize failed"
cd "$MOUNT_POINT"
sha256sum -c /tmp/before.sha || fail "file contents changed"
cd /
umount "$MOUNT_POINT"
/sbin/losetup -d /dev/loop0
echo "BLOCKDEV_RESIZED"

echo ""
echo "=== VM FINISHED ==="
sync
sleep 1
echo o > /proc/sysrq-trigger
sleep 999
INIT_SCRIPT
    chmod +x "$overlay_dir/init"

    # cpio's exit status and its errors must reach set -e. Piped straight into
    # gzip, a missing or failing cpio produced an empty overlay, and the VM
    # stopped at "Mounting boot media failed" with nothing to say why.
    (cd "$overlay_dir" && find . | cpio --quiet -o -H newc) > "$OVERLAY_CPIO"
    gzip -f "$OVERLAY_CPIO"
    cat "$INITRD" "$OVERLAY_CPIO.gz" > "$COMBINED_INITRD"
    rm -rf "$overlay_dir"
}

echo "Preparing block device VM test..."
create_overlay

> "$SERIAL_LOG"

KVM_OPTS=""
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    KVM_OPTS="-enable-kvm -cpu host"
    echo "Using KVM acceleration"
else
    echo "KVM not available, using TCG (slower)"
fi

echo "Booting VM (timeout: ${VM_TIMEOUT}s)..."
set +e
timeout "$VM_TIMEOUT" qemu-system-x86_64 \
    $KVM_OPTS \
    -m 1024 \
    -nographic \
    -kernel "$KERNEL" \
    -initrd "$COMBINED_INITRD" \
    -append "console=ttyS0 quiet" \
    -no-reboot \
    2>&1 | tee "$SERIAL_LOG"
QEMU_EXIT=${PIPESTATUS[0]}
set -e

echo ""
echo "VM finished (exit code: $QEMU_EXIT)"

if grep -q "GUEST_ERROR" "$SERIAL_LOG"; then
    echo "Guest reported an error:"
    grep "GUEST_ERROR" "$SERIAL_LOG"
    exit 1
fi

for marker in BLOCKDEV_MOUNTED_REFUSED BLOCKDEV_BUSY_REFUSED BLOCKDEV_RESIZED "=== VM FINISHED ==="; do
    if ! grep -q "$marker" "$SERIAL_LOG"; then
        echo "ERROR: the guest never reached $marker (timeout or crash)"
        exit 1
    fi
done

echo "SUCCESS: block device resize test passed"
exit 0
