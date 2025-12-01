#!/bin/bash
#
# Run FAT32 test entirely inside a QEMU VM
#
# Creates a cpio overlay with:
# - fat32expander binary
# - test script
# - custom init
#
# The VM creates a FAT32 image in tmpfs, populates it, resizes with
# fat32expander, and verifies - all using the real Linux vfat driver.
#
# Usage: run-vm.sh <work_dir> <test_script> <binary> <kernel> <initrd> <timeout>
#                  <initial_size_mb> <final_size_mb>
#

set -e

WORK_DIR="$1"
TEST_SCRIPT="$2"
BINARY="$3"
KERNEL="$4"
INITRD="$5"
VM_TIMEOUT="${6:-120}"
INITIAL_SIZE_MB="${7:-128}"
FINAL_SIZE_MB="${8:-256}"
SECTOR_SIZE="${9:-512}"

if [ -z "$WORK_DIR" ] || [ -z "$TEST_SCRIPT" ] || [ -z "$BINARY" ]; then
    echo "Usage: $0 <work_dir> <test_script> <binary> <kernel> <initrd> [timeout] [initial_mb] [final_mb] [sector_size]"
    exit 1
fi

SERIAL_LOG="$WORK_DIR/vm-console.log"
OVERLAY_CPIO="$WORK_DIR/overlay.cpio"
COMBINED_INITRD="$WORK_DIR/combined-initrd"

# Download Alpine package if not cached
download_alpine_pkg() {
    local pkg="$1"
    local cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/fat32expander-test/apk"
    local pkg_file="$cache_dir/$pkg.apk"

    mkdir -p "$cache_dir"

    if [ ! -f "$pkg_file" ]; then
        echo "Downloading Alpine package: $pkg..." >&2
        local url="https://dl-cdn.alpinelinux.org/alpine/v3.20/main/x86_64"
        # Get package index to find exact filename
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

    # Alpine .apk files are gzipped tar archives
    # Use --warning=no-unknown-keyword to suppress APK-specific header warnings
    tar -xzf "$pkg_file" -C "$dest_dir" --warning=no-unknown-keyword 2>&1 | grep -v "Ignoring unknown" || true
}

# Create the cpio overlay with our init, binary, and test script
create_overlay() {
    local overlay_dir="$WORK_DIR/overlay"
    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir"/{bin,sbin,usr/sbin,scripts}

    # Copy fat32expander binary
    cp "$BINARY" "$overlay_dir/bin/fat32expander"
    chmod +x "$overlay_dir/bin/fat32expander"

    # Copy test script
    cp "$TEST_SCRIPT" "$overlay_dir/scripts/test.sh"
    chmod +x "$overlay_dir/scripts/test.sh"

    # Download and extract dosfstools from Alpine
    echo "Adding dosfstools to overlay..."
    local dosfstools_pkg=$(download_alpine_pkg "dosfstools")
    extract_alpine_pkg "$dosfstools_pkg" "$overlay_dir"
    # Verify extraction worked
    if [ -f "$overlay_dir/sbin/fsck.fat" ]; then
        chmod +x "$overlay_dir/sbin/"*
        echo "  dosfstools extracted successfully"
    else
        echo "ERROR: Failed to extract dosfstools"
        ls -la "$overlay_dir/"
    fi

    # Download and extract losetup from Alpine (full-featured, supports -b for sector size)
    # Also need libsmartcols which losetup depends on
    echo "Adding losetup to overlay..."
    local libsmartcols_pkg=$(download_alpine_pkg "libsmartcols")
    extract_alpine_pkg "$libsmartcols_pkg" "$overlay_dir"
    local losetup_pkg=$(download_alpine_pkg "losetup")
    extract_alpine_pkg "$losetup_pkg" "$overlay_dir"
    if [ -f "$overlay_dir/sbin/losetup" ]; then
        chmod +x "$overlay_dir/sbin/losetup"
        echo "  losetup extracted successfully"
    else
        echo "ERROR: Failed to extract losetup"
    fi

    # Create custom init that runs AFTER Alpine's init sets up busybox
    # We use /etc/local.d/ for Alpine's openrc, but since we're using
    # the minimal initramfs, we'll replace /init entirely
    cat > "$overlay_dir/init" << 'INIT_SCRIPT'
#!/bin/busybox sh
# Custom init for FAT32 testing
# This runs as PID 1

# Mount essential filesystems FIRST so we have /dev
/bin/busybox mkdir -p /proc /sys /dev /tmp /mnt
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t tmpfs tmpfs /tmp

# Now redirect to serial console (devtmpfs should have created ttyS0)
exec > /dev/ttyS0 2>&1

echo "=== FAT32 Test VM Starting ==="

# Set up busybox symlinks
/bin/busybox --install -s /bin 2>/dev/null || true

# Create loop device nodes if needed
[ -e /dev/loop0 ] || mknod /dev/loop0 b 7 0
[ -e /dev/loop1 ] || mknod /dev/loop1 b 7 1
[ -e /dev/loop2 ] || mknod /dev/loop2 b 7 2

# Get parameters from kernel command line
INITIAL_SIZE_MB=$(cat /proc/cmdline | tr ' ' '\n' | grep '^initial_size=' | cut -d= -f2)
FINAL_SIZE_MB=$(cat /proc/cmdline | tr ' ' '\n' | grep '^final_size=' | cut -d= -f2)
SECTOR_SIZE=$(cat /proc/cmdline | tr ' ' '\n' | grep '^sector_size=' | cut -d= -f2)
INITIAL_SIZE_MB=${INITIAL_SIZE_MB:-128}
FINAL_SIZE_MB=${FINAL_SIZE_MB:-256}
SECTOR_SIZE=${SECTOR_SIZE:-512}

echo "Test parameters:"
echo "  Initial size: ${INITIAL_SIZE_MB}MB"
echo "  Final size: ${FINAL_SIZE_MB}MB"
echo "  Sector size: ${SECTOR_SIZE} bytes"

# Create FAT32 image in tmpfs
FAT_IMAGE="/tmp/test.img"
MOUNT_POINT="/mnt/fat"
mkdir -p "$MOUNT_POINT"

echo ""
echo "=== Phase 1: Creating ${INITIAL_SIZE_MB}MB FAT32 image (sector size: ${SECTOR_SIZE}) ==="
dd if=/dev/zero of="$FAT_IMAGE" bs=1M count="$INITIAL_SIZE_MB" 2>/dev/null
/sbin/mkfs.fat -F 32 -S "$SECTOR_SIZE" "$FAT_IMAGE" >/dev/null

echo "=== Phase 2: Mounting and populating ==="
# For non-512 sector sizes, use full losetup with -b flag to set sector size
if [ "$SECTOR_SIZE" -ne 512 ]; then
    echo "Using losetup with ${SECTOR_SIZE}-byte sector size"
    /sbin/losetup -b "$SECTOR_SIZE" /dev/loop0 "$FAT_IMAGE"
    if [ $? -ne 0 ]; then
        echo "GUEST_ERROR: Failed to setup loop device with sector size $SECTOR_SIZE"
        poweroff -f
    fi
    mount -t vfat -o iocharset=utf8,shortname=mixed /dev/loop0 "$MOUNT_POINT"
else
    mount -t vfat -o loop,iocharset=utf8,shortname=mixed "$FAT_IMAGE" "$MOUNT_POINT"
fi
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: Failed to mount FAT32"
    poweroff -f
fi

echo "FAT32 mounted at $MOUNT_POINT"

# Run the populate portion of the test script
cd "$MOUNT_POINT"
echo "=== Running test script (populate) ==="
/bin/sh /scripts/test.sh populate "$MOUNT_POINT"

# Generate pre-resize checksums
echo ""
echo "=== PRE_CHECKSUMS_START ==="
find . -type f -exec sha256sum {} \; 2>/dev/null | sort
echo "=== PRE_CHECKSUMS_END ==="

# Sync and unmount
cd /
sync
sleep 1
umount "$MOUNT_POINT"
# Detach loop device if we used one explicitly
if [ "$SECTOR_SIZE" -ne 512 ]; then
    /sbin/losetup -d /dev/loop0
fi
echo "FAT32 unmounted"

echo ""
echo "=== Phase 3: Extending image to ${FINAL_SIZE_MB}MB ==="
dd if=/dev/zero bs=1M count=$((FINAL_SIZE_MB - INITIAL_SIZE_MB)) >> "$FAT_IMAGE" 2>/dev/null
ls -la "$FAT_IMAGE"

echo ""
echo "=== Phase 4: Running fat32expander ==="
/bin/fat32expander resize --verbose --force "$FAT_IMAGE"
RESIZE_EXIT=$?

if [ $RESIZE_EXIT -ne 0 ]; then
    echo "GUEST_ERROR: fat32expander failed with exit code $RESIZE_EXIT"
    poweroff -f
fi

echo ""
echo "=== Phase 5: Verifying with fsck ==="
/sbin/fsck.fat -n "$FAT_IMAGE"
FSCK_EXIT=$?
if [ $FSCK_EXIT -ne 0 ]; then
    echo "GUEST_ERROR: fsck.fat failed with exit code $FSCK_EXIT"
    poweroff -f
fi
echo "fsck.fat passed"

echo ""
echo "=== Phase 6: Mounting and verifying checksums ==="
if [ "$SECTOR_SIZE" -ne 512 ]; then
    /sbin/losetup -b "$SECTOR_SIZE" /dev/loop0 "$FAT_IMAGE"
    mount -t vfat -o iocharset=utf8,shortname=mixed /dev/loop0 "$MOUNT_POINT"
else
    mount -t vfat -o loop,iocharset=utf8,shortname=mixed "$FAT_IMAGE" "$MOUNT_POINT"
fi
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: Failed to mount FAT32 after resize"
    poweroff -f
fi

cd "$MOUNT_POINT"
echo "=== POST_CHECKSUMS_START ==="
find . -type f -exec sha256sum {} \; 2>/dev/null | sort
echo "=== POST_CHECKSUMS_END ==="

# File count
FILE_COUNT=$(find . -type f | wc -l)
echo "FILE_COUNT: $FILE_COUNT"

cd /
umount "$MOUNT_POINT"
if [ "$SECTOR_SIZE" -ne 512 ]; then
    /sbin/losetup -d /dev/loop0
fi

echo ""
echo "=== VM FINISHED ==="
sync
sleep 1
# Use sysrq for immediate power-off (works without init system)
# Sleep keeps init alive while kernel processes the power-off
echo o > /proc/sysrq-trigger
sleep 999
INIT_SCRIPT
    chmod +x "$overlay_dir/init"

    # Create the cpio archive and compress it
    # (kernel supports concatenated compressed cpio archives)
    (cd "$overlay_dir" && find . | cpio -o -H newc 2>/dev/null) | gzip > "$OVERLAY_CPIO.gz"

    # Combine with Alpine initramfs (both are gzip compressed)
    cat "$INITRD" "$OVERLAY_CPIO.gz" > "$COMBINED_INITRD"

    rm -rf "$overlay_dir"
    echo "Created combined initramfs: $COMBINED_INITRD"
}

echo "Preparing VM test environment..."
echo "  Work dir: $WORK_DIR"
echo "  Test script: $TEST_SCRIPT"
echo "  Binary: $BINARY"
echo "  Initial size: ${INITIAL_SIZE_MB}MB"
echo "  Final size: ${FINAL_SIZE_MB}MB"
echo "  Sector size: ${SECTOR_SIZE} bytes"
echo "  Timeout: ${VM_TIMEOUT}s"

# Create the overlay
create_overlay

# Clear previous log
> "$SERIAL_LOG"

# Determine if KVM is available
KVM_OPTS=""
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    KVM_OPTS="-enable-kvm -cpu host"
    echo "Using KVM acceleration"
else
    echo "KVM not available, using TCG (slower)"
fi

# Run QEMU
# -nographic redirects serial to stdio automatically
echo "Booting VM..."
timeout "$VM_TIMEOUT" qemu-system-x86_64 \
    $KVM_OPTS \
    -m 1024 \
    -nographic \
    -kernel "$KERNEL" \
    -initrd "$COMBINED_INITRD" \
    -append "console=ttyS0 quiet initial_size=$INITIAL_SIZE_MB final_size=$FINAL_SIZE_MB sector_size=$SECTOR_SIZE" \
    -no-reboot \
    2>&1 | tee "$SERIAL_LOG"

QEMU_EXIT=$?

echo ""
echo "VM finished (exit code: $QEMU_EXIT)"

# Check for errors in output
if grep -q "GUEST_ERROR" "$SERIAL_LOG"; then
    echo "Guest reported an error:"
    grep "GUEST_ERROR" "$SERIAL_LOG"
    exit 1
fi

# Check if VM finished normally
if ! grep -q "=== VM FINISHED ===" "$SERIAL_LOG"; then
    echo "ERROR: VM did not finish normally (timeout or crash)"
    exit 1
fi

# Extract and compare checksums
PRE_CHECKSUMS=$(sed -n '/=== PRE_CHECKSUMS_START ===/,/=== PRE_CHECKSUMS_END ===/p' "$SERIAL_LOG" | grep -v '===')
POST_CHECKSUMS=$(sed -n '/=== POST_CHECKSUMS_START ===/,/=== POST_CHECKSUMS_END ===/p' "$SERIAL_LOG" | grep -v '===')

echo "$PRE_CHECKSUMS" > "$WORK_DIR/pre-checksums.txt"
echo "$POST_CHECKSUMS" > "$WORK_DIR/post-checksums.txt"

PRE_COUNT=$(echo "$PRE_CHECKSUMS" | grep -c . || echo 0)
POST_COUNT=$(echo "$POST_CHECKSUMS" | grep -c . || echo 0)

echo "Pre-resize files: $PRE_COUNT"
echo "Post-resize files: $POST_COUNT"

if [ "$PRE_COUNT" -eq 0 ]; then
    echo "ERROR: No pre-resize checksums captured"
    exit 1
fi

if ! diff -q "$WORK_DIR/pre-checksums.txt" "$WORK_DIR/post-checksums.txt" >/dev/null 2>&1; then
    echo "ERROR: Checksum mismatch!"
    diff "$WORK_DIR/pre-checksums.txt" "$WORK_DIR/post-checksums.txt"
    exit 1
fi

echo "SUCCESS: All $PRE_COUNT files verified"
exit 0
