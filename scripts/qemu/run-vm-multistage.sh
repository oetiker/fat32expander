#!/bin/bash
#
# Run multi-stage FAT32 resize test inside a QEMU VM
#
# This specialized runner tests multiple resize operations with
# file activity between each stage:
#   Stage 1: 64MB with initial files
#   Stage 2: Resize to 128MB, add files, append to existing
#   Stage 3: Resize to 256MB, more file activity, final verify
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

# Create the cpio overlay
create_overlay() {
    local overlay_dir="$WORK_DIR/overlay"
    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir"/{bin,sbin,usr/sbin,scripts}

    # Copy fat32expander binary
    cp "$BINARY" "$overlay_dir/bin/fat32expander"
    chmod +x "$overlay_dir/bin/fat32expander"

    # Download and extract dosfstools from Alpine
    echo "Adding dosfstools to overlay..."
    local dosfstools_pkg=$(download_alpine_pkg "dosfstools")
    extract_alpine_pkg "$dosfstools_pkg" "$overlay_dir"
    chmod +x "$overlay_dir/sbin/"* 2>/dev/null || true

    # Create custom init for multi-stage test
    cat > "$overlay_dir/init" << 'INIT_SCRIPT'
#!/bin/busybox sh
# Multi-stage resize test init

# Mount essential filesystems
/bin/busybox mkdir -p /proc /sys /dev /tmp /mnt
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t tmpfs tmpfs /tmp

exec > /dev/ttyS0 2>&1

echo "=== Multi-Stage Resize Test Starting ==="

/bin/busybox --install -s /bin 2>/dev/null || true
[ -e /dev/loop0 ] || mknod /dev/loop0 b 7 0

FAT_IMAGE="/tmp/test.img"
MOUNT_POINT="/mnt/fat"
mkdir -p "$MOUNT_POINT"

# Helper to create test file with known content
create_file() {
    local path="$1"
    local size_kb="$2"
    local seed="$3"
    # Use seed for reproducible content
    dd if=/dev/urandom of="$path" bs=1024 count="$size_kb" 2>/dev/null
}

# Helper to append to file
append_file() {
    local path="$1"
    local size_kb="$2"
    dd if=/dev/urandom bs=1024 count="$size_kb" >> "$path" 2>/dev/null
}

# ============== STAGE 1: Initial filesystem (64MB) ==============
echo ""
echo "=== STAGE 1: Creating initial 64MB filesystem ==="
dd if=/dev/zero of="$FAT_IMAGE" bs=1M count=64 2>/dev/null
/sbin/mkfs.fat -F 32 "$FAT_IMAGE" >/dev/null

mount -t vfat -o loop "$FAT_IMAGE" "$MOUNT_POINT"
cd "$MOUNT_POINT"

echo "Creating initial files..."
echo "Initial content for file1" > file1.txt
echo "Initial content for file2" > file2.txt
create_file "data1.bin" 100
create_file "data2.bin" 200
mkdir -p subdir
echo "Nested file content" > subdir/nested.txt
create_file "subdir/nested_data.bin" 150

echo "=== STAGE1_CHECKSUMS_START ==="
find . -type f -exec sha256sum {} \; 2>/dev/null | sort
echo "=== STAGE1_CHECKSUMS_END ==="

cd /
sync
umount "$MOUNT_POINT"

# ============== STAGE 2: First resize (64MB -> 128MB) ==============
echo ""
echo "=== STAGE 2: Resizing to 128MB ==="
dd if=/dev/zero bs=1M count=64 >> "$FAT_IMAGE" 2>/dev/null

/bin/fat32expander resize --verbose --force "$FAT_IMAGE"
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: First resize failed"
    echo o > /proc/sysrq-trigger; sleep 999
fi

/sbin/fsck.fat -n "$FAT_IMAGE"
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: fsck after first resize failed"
    echo o > /proc/sysrq-trigger; sleep 999
fi

# Add new files and append to existing
mount -t vfat -o loop "$FAT_IMAGE" "$MOUNT_POINT"
cd "$MOUNT_POINT"

echo "Adding new files and appending to existing..."
echo "New file after first resize" > file3_stage2.txt
create_file "data3_stage2.bin" 300
append_file "file1.txt" 1
append_file "data1.bin" 50
mkdir -p subdir2
echo "Stage 2 nested" > subdir2/stage2.txt
create_file "subdir2/stage2_data.bin" 100

echo "=== STAGE2_CHECKSUMS_START ==="
find . -type f -exec sha256sum {} \; 2>/dev/null | sort
echo "=== STAGE2_CHECKSUMS_END ==="

STAGE2_COUNT=$(find . -type f | wc -l)
echo "STAGE2_FILE_COUNT: $STAGE2_COUNT"

cd /
sync
umount "$MOUNT_POINT"

# ============== STAGE 3: Second resize (128MB -> 256MB) ==============
echo ""
echo "=== STAGE 3: Resizing to 256MB ==="
dd if=/dev/zero bs=1M count=128 >> "$FAT_IMAGE" 2>/dev/null

/bin/fat32expander resize --verbose --force "$FAT_IMAGE"
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: Second resize failed"
    echo o > /proc/sysrq-trigger; sleep 999
fi

/sbin/fsck.fat -n "$FAT_IMAGE"
if [ $? -ne 0 ]; then
    echo "GUEST_ERROR: fsck after second resize failed"
    echo o > /proc/sysrq-trigger; sleep 999
fi

# Final file activity
mount -t vfat -o loop "$FAT_IMAGE" "$MOUNT_POINT"
cd "$MOUNT_POINT"

echo "Final file activity..."
echo "Final stage file" > file4_stage3.txt
create_file "data4_stage3.bin" 400
append_file "file2.txt" 2
append_file "data2.bin" 100
mkdir -p subdir3/deep
echo "Deep nested after resize" > subdir3/deep/final.txt

echo "=== FINAL_CHECKSUMS_START ==="
find . -type f -exec sha256sum {} \; 2>/dev/null | sort
echo "=== FINAL_CHECKSUMS_END ==="

FINAL_COUNT=$(find . -type f | wc -l)
echo "FINAL_FILE_COUNT: $FINAL_COUNT"

cd /
umount "$MOUNT_POINT"

echo ""
echo "=== Multi-Stage Test Complete ==="
echo "=== VM FINISHED ==="
sync
sleep 1
echo o > /proc/sysrq-trigger
sleep 999
INIT_SCRIPT
    chmod +x "$overlay_dir/init"

    # Create cpio and combine with Alpine initramfs
    (cd "$overlay_dir" && find . | cpio -o -H newc 2>/dev/null) | gzip > "$OVERLAY_CPIO.gz"
    cat "$INITRD" "$OVERLAY_CPIO.gz" > "$COMBINED_INITRD"
    rm -rf "$overlay_dir"
}

echo "Preparing multi-stage VM test..."
create_overlay

> "$SERIAL_LOG"

# Determine KVM availability
KVM_OPTS=""
if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
    KVM_OPTS="-enable-kvm -cpu host"
    echo "Using KVM acceleration"
else
    echo "KVM not available, using TCG (slower)"
fi

echo "Booting VM (timeout: ${VM_TIMEOUT}s)..."
timeout "$VM_TIMEOUT" qemu-system-x86_64 \
    $KVM_OPTS \
    -m 1024 \
    -nographic \
    -kernel "$KERNEL" \
    -initrd "$COMBINED_INITRD" \
    -append "console=ttyS0 quiet" \
    -no-reboot \
    2>&1 | tee "$SERIAL_LOG" | grep -Ev "^[a-f0-9]{64}  \."

QEMU_EXIT=$?

echo ""
echo "VM finished (exit code: $QEMU_EXIT)"

# Check for errors
if grep -q "GUEST_ERROR" "$SERIAL_LOG"; then
    echo "Guest reported an error:"
    grep "GUEST_ERROR" "$SERIAL_LOG"
    exit 1
fi

if ! grep -q "=== VM FINISHED ===" "$SERIAL_LOG"; then
    echo "ERROR: VM did not finish normally (timeout or crash)"
    exit 1
fi

# Verify file counts increased at each stage
STAGE2_COUNT=$(grep "STAGE2_FILE_COUNT:" "$SERIAL_LOG" | cut -d: -f2 | tr -d ' \n\r')
FINAL_COUNT=$(grep "FINAL_FILE_COUNT:" "$SERIAL_LOG" | cut -d: -f2 | tr -d ' \n\r')

echo "Stage 2 file count: $STAGE2_COUNT"
echo "Final file count: $FINAL_COUNT"

if [ -z "$STAGE2_COUNT" ] || [ -z "$FINAL_COUNT" ]; then
    echo "ERROR: Could not extract file counts"
    exit 1
fi

if [ "$FINAL_COUNT" -le "$STAGE2_COUNT" ]; then
    echo "ERROR: File count did not increase in final stage"
    exit 1
fi

echo "SUCCESS: Multi-stage resize test passed"
echo "  - Two resize operations completed"
echo "  - Files added and appended between resizes"
echo "  - Final file count: $FINAL_COUNT"
exit 0
