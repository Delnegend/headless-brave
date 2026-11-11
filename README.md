<div align="center">

# Headless Brave

**Run Brave Origin in a container with a VNC desktop, an in-browser client, and DevTools Protocol access for AI agents.**

[![CI](https://img.shields.io/github/actions/workflow/status/Delnegend/headless-brave/ci.yml?branch=main&style=flat-square)](https://github.com/Delnegend/headless-brave/actions)
[![Release](https://img.shields.io/github/v/release/Delnegend/headless-brave?style=flat-square)](https://github.com/Delnegend/headless-brave/releases)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)

</div>

---

## Quick Start

Get running in less than 60 seconds:

```bash
# 1. Pull published image
podman pull ghcr.io/delnegend/headless-brave:latest

# 2. Run container
podman run -d --name headless-brave --shm-size=2g \
  -p 127.0.0.1:8000:8000 -p 127.0.0.1:5900:5900 -p 127.0.0.1:9222:9222 \
  -v headless-brave-data:/data -v headless-brave-opt:/opt \
  ghcr.io/delnegend/headless-brave:latest
```

Visit `http://127.0.0.1:8000` to view the desktop, or connect agents via `ws://127.0.0.1:9222`.

## Highlights

- **Persistent automation**: Browser and virtual screen run continuously, keeping CDP connections alive whether or not anyone is watching.
- **Shared non-intrusive viewing**: Watch automation live via web UI (`:8000`) or VNC (`:5900`) without stealing focus from the agent.
- **Unprivileged sandbox confinement**: Runs as UID 1000 with Chromium's native user-namespace sandbox enabled; renderers stay isolated.
- **Self-updating browser engine**: Automatically polls upstream Brave Origin releases every 6 hours and updates `/opt` without losing container state.
- **Zero external runtime dependencies**: Powered by a single static Rust binary (`headless-brave-web`) embedding web assets and WebSocket relays.

## Documentation

- **[Architecture](docs/architecture.md)**: System supervisor, WebSocket bridges, update cycles, and troubleshooting.
- **[Development](docs/development.md)**: Running checks, build steps, and smoke tests.
- **[Deployment](docs/deployment.md)**: Container deployment via Podman or Docker Compose, smoke verification, and upgrades.

## License

[MIT](LICENSE)
