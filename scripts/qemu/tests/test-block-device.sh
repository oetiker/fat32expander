#!/bin/bash
# Test: Resize a partition of a real block device
#
# Every other scenario resizes an image file. This one resizes /dev/loop0p1,
# a partition of a loop device, after the partition has grown: the tool's
# main use. It also checks that a mounted or otherwise busy partition is
# refused with exit 4. See scripts/qemu/run-vm-blockdev.sh.

TEST_DESCRIPTION="Resize a grown partition of a loop device; refuse a mounted or busy one"
TEST_DATA_TYPE="blockdev"
TEST_TIMEOUT=300
