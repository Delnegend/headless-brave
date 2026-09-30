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
# Defaults; every one of them is overridable at run time.
ENV RESOLUTION=1920x1080
ENV VNC_DISPLAY=:99
ENV VNC_PASSWORD=headless
ENV VNC_PORT=5900
ENV WEB_PORT=80
ENV CDP_PORT=9222
ENV BRAVE_PROFILE=/data/profile
ENV BRAVE_ROOT=/opt/brave.com
ENV BRAVE_UPGRADE=0
ENV RUST_LOG=headless_brave_web=info,warn

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

# Brave itself is installed at container start onto a volume, so a new release
# costs a restart rather than a rebuild. Only the shared libraries it links
# against are baked in here — they move far less often than the browser — and
# the payload under /opt is dropped again so the image does not carry a copy
# that the volume immediately shadows.
COPY --from=brave-key /brave-browser-archive-keyring.gpg /usr/share/keyrings/
RUN echo "deb [arch=amd64 signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg] https://brave-browser-apt-release.s3.brave.com/ stable main" \
    > /etc/apt/sources.list.d/brave-browser-release.list \
    && apt-get update && apt-get install -y --no-install-recommends brave-browser \
    && rm -rf /opt/brave.com /etc/cron.daily/brave-browser \
    && rm -rf /var/lib/apt/lists/*

COPY --from=build /src/target/release/headless-brave-web /usr/local/bin/headless-brave-web

EXPOSE 80 5900 9222

# The binary is the whole container: it starts the screen, the window manager,
# the browser, x11vnc and the web service, and takes the container down if any
# of them stops.
CMD ["/usr/local/bin/headless-brave-web"]
