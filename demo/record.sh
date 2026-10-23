#!/bin/sh
# Record docs/demo/fat32expander.webp from demo/fat32expander.toml. Run
# through `make demo`, which builds the release binary and passes its
# directory as $1. Further arguments go to `ansidrama record`, for example
# `--dump-png frames/`.
#
# The recording runs in a fresh scratch directory with a new disk.img: a
# 256 MiB FAT32 filesystem holding three photos, a note and a directory.
# Needs mkfs.fat (dosfstools) and mcopy, mmd and mdir (mtools).
set -eu
bindir=$1
shift
demo=$(cd "$(dirname "$0")" && pwd)
scratch=$(mktemp -d "${TMPDIR:-/tmp}/fat32expander-demo.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

cd "$scratch"
truncate -s 256M disk.img
mkfs.fat -F 32 -n PHOTOS disk.img >/dev/null
for i in 1 2 3; do
    head -c 3M /dev/urandom > "IMG_000$i.JPG"
done
echo "holiday 2026" > notes.txt
mcopy -i disk.img IMG_0001.JPG IMG_0002.JPG IMG_0003.JPG notes.txt ::/
mmd -i disk.img ::/2026
rm IMG_000?.JPG notes.txt

# The shell starts here, next to disk.img; ansidrama resolves `out` against
# the directory of the script, so the WebP still lands in docs/demo/.
env PATH="$bindir:$PATH" \
    "${ANSIDRAMA:-ansidrama}" record "$demo/fat32expander.toml" "$@"
