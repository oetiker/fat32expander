#!/bin/bash
# Test: Fragmented files resize
#
# This test creates heavily fragmented files by:
# 1. Creating multiple files
# 2. Extending them in rotation (file1, file2, file3, file1, ...)
#
# This causes each file's clusters to be scattered across the disk,
# testing that FAT chains are preserved correctly during resize.

TEST_DESCRIPTION="Heavily fragmented files (non-contiguous clusters)"
TEST_IMAGE_SIZE_MB=64
TEST_RESIZE_TO_MB=128
TEST_DATA_TYPE="fragmented"
