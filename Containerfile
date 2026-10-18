# The environment for `make ci-heavy`: the tests that need mkfs.fat, dosfsck,
# mtools and QEMU. scripts/ci-container.sh builds and runs it, in CI and on a
# developer machine alike. The image holds tools only; the checkout is
# mounted at /src.
FROM docker.io/library/rust:1-trixie

# Without it, apt-get fills the build log with "debconf: unable to initialize
# frontend: Dialog" for each package; the build has no terminal to ask on.
ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      cpio curl dosfstools mtools musl-tools qemu-system-x86 \
 && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-musl \
 && rustup component add clippy rustfmt
