FROM debian:13-slim

ARG MOLD_VERSION=2.42.0

ENV LANG=C.UTF-8
ENV LC_ALL=C.UTF-8
ENV SHELL=/bin/bash

# The toolchain plus podman: the image under test is built and run from here,
# and the Rust service is rebuilt on the host for a quick edit-compile loop.
RUN --mount=type=cache,target=/var/cache/apt \
    apt-get update && apt-get install -y --no-install-recommends \
    build-essential ca-certificates curl git openssh-client \
    clang sudo pkg-config musl-tools \
    podman fuse-overlayfs slirp4netns iproute2 jq \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1000 headless-brave && \
    useradd -m -u 1000 -g headless-brave -s /bin/bash headless-brave && \
    echo "headless-brave ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/headless-brave && \
    chmod 0440 /etc/sudoers.d/headless-brave && \
    mkdir -p /usr/local/cargo && \
    chown -R headless-brave:headless-brave /usr/local/cargo && \
    printf 'export PATH=/usr/local/cargo/bin:${PATH}\n' > /etc/profile.d/headless-brave-path.sh

SHELL ["/bin/bash", "-c"]
USER headless-brave

ENV CARGO_HOME=/usr/local/cargo
ENV PATH="/usr/local/cargo/bin:${PATH}"

# Rust stable toolchain (minimal) + analyzer/fmt/clippy + musl target —
# pre-installed so the container works even if postCreateCommand is skipped
# (e.g. Zed). Native build stays gnu.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal --no-modify-path && \
    /usr/local/cargo/bin/rustup component add rust-analyzer rustfmt clippy && \
    /usr/local/cargo/bin/rustup target add x86_64-unknown-linux-musl

# mold — pinned so the linker is fast and the version reproducible, and
# configured here so native (gnu) builds use it without a project-level
# .cargo/config.toml.
RUN curl -fsSL -o /tmp/mold.tar.gz "https://github.com/rui314/mold/releases/download/v${MOLD_VERSION}/mold-${MOLD_VERSION}-x86_64-linux.tar.gz" && \
    echo "f5ed2f6e31d1ada4f07fe766fe0de7a73104d1c5cdc59086fcecc16a43720b6d  /tmp/mold.tar.gz" | sha256sum -c - && \
    tar -xf /tmp/mold.tar.gz -C /usr/local/cargo "mold-${MOLD_VERSION}-x86_64-linux" && \
    rm /tmp/mold.tar.gz && \
    printf '[target.x86_64-unknown-linux-gnu]\nlinker = "clang"\nrustflags = ["-C", "link-arg=-fuse-ld=/usr/local/cargo/mold-%s-x86_64-linux/bin/mold"]\n' "$MOLD_VERSION" > /usr/local/cargo/config.toml

# cargo-binstall + flamegraph (cached in image, not postinstall)
RUN curl -L --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh | bash && \
    cargo binstall -y flamegraph
