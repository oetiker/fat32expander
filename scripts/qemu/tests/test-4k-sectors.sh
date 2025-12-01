#!/bin/bash
# Test: Resize with 4096-byte sectors (4K native)
#
# This test verifies that fat32expander correctly handles FAT32
# filesystems with 4096-byte sectors, which are used on 4Kn drives
# (particularly for EFI System Partitions on modern SSDs).
#
# Note: FAT32 requires >= 65525 clusters. With 4K sectors and 1 sector/cluster,
# the minimum size for valid FAT32 is ~256MB. We use 512MB to ensure proper
# FAT32 layout (total_sectors_32 field is used, not total_sectors_16).

TEST_DESCRIPTION="Resize with 4K sector FAT32 filesystem (EFI partition scenario)"
TEST_IMAGE_SIZE_MB=512
TEST_RESIZE_TO_MB=1024
TEST_DATA_TYPE="simple"
TEST_SECTOR_SIZE=4096
TEST_TIMEOUT=180  # Larger images need more time
