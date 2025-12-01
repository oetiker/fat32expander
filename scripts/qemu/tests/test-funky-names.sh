#!/bin/bash
# Test: Unicode and special character filenames
#
# This test creates files with:
# - Long filenames (LFN > 8.3 format)
# - Unicode characters (German umlauts, Japanese, Russian, Greek)
# - Special characters (dashes, underscores, dots)
# - Case variations

TEST_DESCRIPTION="Long filenames, Unicode, and special characters"
TEST_IMAGE_SIZE_MB=128
TEST_RESIZE_TO_MB=256
TEST_DATA_TYPE="funky"
