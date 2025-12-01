#!/bin/bash
# Test: Large directory spanning multiple clusters
#
# This test creates a directory with many files, causing the directory
# itself to span multiple clusters. Each FAT32 directory entry is 32 bytes,
# so with 512-byte clusters, we get 16 entries per cluster.
# With LFN (long filenames), each file can use multiple directory entries.
#
# This tests that directory clusters are properly shifted during resize.

TEST_DESCRIPTION="Large directory spanning multiple clusters (500+ files)"
TEST_IMAGE_SIZE_MB=64
TEST_RESIZE_TO_MB=128
TEST_DATA_TYPE="largedir"
