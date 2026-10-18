#!/bin/bash
# Build the Containerfile image and run a make target in it, with the checkout
# mounted at /src. CI runs `scripts/ci-container.sh ci-heavy`; the same command
# reproduces the run on a developer machine.
#
# Files the tests write into the mounted checkout belong to the calling user:
# rootless podman maps the container's root to that user, and docker runs as
# that user. The tests that skip under root are plain tests, run by ri-ci-rust
# outside the container; ci-heavy runs only the ignored ones and the scripts.
# /dev/kvm, when the host has it, is passed in with its group so QEMU can use
# KVM instead of the much slower TCG.
#
# CARGO_BUILD_JOBS and RUST_TEST_THREADS pass through when set. CONTAINER_OPTS
# adds options to the run, for example CONTAINER_OPTS="--cpus=4 --memory=6g"
# on a shared machine.
set -euo pipefail

TARGET="${1:-ci-heavy}"
IMAGE=fat32expander-ci
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if command -v podman >/dev/null 2>&1; then
    ENGINE=podman
    # No --userns=keep-id: a podman whose storage cannot remap the image
    # layers refuses every custom mapping with "make .../merged private:
    # Permission denied" (podman 4.9.3, storage under /opt).
    USER_OPTS=()
    # Rootless podman cannot map a host group by number; keep-groups hands
    # the calling user's own groups, kvm among them, to the container.
    KVM_GROUP=keep-groups
else
    ENGINE=docker
    USER_OPTS=(--user "$(id -u):$(id -g)")
    KVM_GROUP="$(stat -c %g /dev/kvm 2>/dev/null || true)"
fi

# The image needs no build context: it copies nothing from the tree.
"$ENGINE" build -t "$IMAGE" - <"$ROOT/Containerfile"

# shellcheck disable=SC2206 # CONTAINER_OPTS is a list of words on purpose
EXTRA_OPTS=(${CONTAINER_OPTS:-})

KVM_OPTS=()
if [ -e /dev/kvm ]; then
    KVM_OPTS=(--device /dev/kvm --group-add "$KVM_GROUP")
    # QEMU falls back to TCG without a word beyond "KVM not available", so say
    # what the container gets.
    echo "ci-container: /dev/kvm $(stat -c '%A %U:%G' /dev/kvm), group-add $KVM_GROUP, caller $(id)"
else
    echo "ci-container: no /dev/kvm on this host, QEMU will use TCG"
fi

exec "$ENGINE" run --rm \
    "${USER_OPTS[@]}" \
    "${KVM_OPTS[@]}" \
    "${EXTRA_OPTS[@]}" \
    -e HOME=/tmp \
    -e CARGO_TERM_COLOR \
    -e CARGO_BUILD_JOBS \
    -e RUST_TEST_THREADS \
    -v "$ROOT:/src" \
    -w /src \
    "$IMAGE" \
    make "$TARGET"
