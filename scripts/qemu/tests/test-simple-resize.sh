#!/bin/bash
# Test: Simple resize without FAT growth
#
# This test verifies basic resize functionality when the FAT tables
# don't need to grow (small size increase).

TEST_DESCRIPTION="Simple resize without FAT table growth"
TEST_IMAGE_SIZE_MB=128
TEST_RESIZE_TO_MB=140
TEST_DATA_TYPE="simple"
