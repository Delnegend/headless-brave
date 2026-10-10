FROM rust:1.98.1-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

# The signing key for Brave's repository is fetched here so that wget and gnupg
# do not have to be in the image that runs.
FROM debian:trixie-slim AS brave-key
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates gnupg wget \
    && wget -O /tmp/key.gpg https://brave-browser-apt-release.s3.brave.com/brave-browser-archive-keyring.gpg \
    && gpg --dearmor -o /brave-browser-archive-keyring.gpg /tmp/key.gpg

FROM debian:trixie-slim

ENV DEBIAN_FRONTEND=noninteractive
ENV LANG=C.UTF-8
# Defaults; every one of them is overridable at run time. VNC_PASSWORD has no
# baked-in value on purpose: it lives only in the binary's and compose's
# fallbacks, so the image layers never carry a credential.
ENV RESOLUTION=1920x1080
ENV VNC_DISPLAY=:99
ENV VNC_PORT=5900
ENV WEB_PORT=8000
ENV CDP_PORT=9222
ENV BRAVE_PROFILE=/data/profile
ENV BRAVE_ROOT=/opt/brave.com
ENV RUST_LOG=headless_brave_web=info,warn

# The container runs as this user, not root. Chromium refuses to start as root
# unless its sandbox is switched off, and the sandbox is worth keeping for
# something browsing the open web. The two mount points are created here and
# handed over so that a fresh named volume inherits the ownership rather than
# arriving root-owned and unusable.
RUN useradd --uid 1000 --user-group --create-home --shell /bin/bash headless \
    && mkdir -p /data /opt \
    && chown headless:headless /data /opt

# xvfb and x11vnc give the browser a desktop that can be watched without
# touching it — any number of viewers share one read-mostly screen, and none of
# them can take it away from the automation driving the browser over CDP.
# novnc is the browser half of that, served by our own process.
RUN --mount=type=cache,target=/var/cache/apt \
    apt-get update && apt-get install -y --no-install-recommends \
    xvfb x11vnc fluxbox novnc \
    fonts-dejavu-core fonts-liberation2 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Brave Origin is installed at container start onto a volume, so a new release
# costs a restart rather than a rebuild. Only the shared libraries it links
# against are baked in here — they move far less often than the browser — and
# the payload under /opt is dropped again so the image does not carry a copy
# that the volume immediately shadows.
COPY --from=brave-key /brave-browser-archive-keyring.gpg /usr/share/keyrings/
RUN echo "deb [arch=amd64 signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg] https://brave-browser-apt-release.s3.brave.com/ stable main" \
    > /etc/apt/sources.list.d/brave-browser-release.list \
    && apt-get update && apt-get install -y --no-install-recommends brave-origin \
    && rm -rf /opt/brave.com /etc/cron.daily/brave-origin \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/headless-brave-web /usr/local/bin/headless-brave-web

EXPOSE 8000 5900 9222

# Nothing in the container needs to be root, so nothing runs as root. Binding a
# privileged port is not available, which is why WEB_PORT is not 80.
USER headless

# The binary is the whole container: it installs the browser, starts the screen,
# the window manager, the browser, x11vnc and the web service, and takes the
# container down if any of them stops.
CMD ["/usr/local/bin/headless-brave-web"]
