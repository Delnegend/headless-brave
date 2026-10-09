# Configuration Reference

All runtime settings are configured through environment variables passed to the container.

## Environment Variables

| Variable | Default | Description |
|---|---|---|
| `RESOLUTION` | `1920x1080` | Size of the virtual screen |
| `BRAVE_PROFILE` | `/data/profile` | Directory where the browser stores profile data (cookies, logins, history) |
| `BRAVE_ROOT` | `/opt/brave.com` | Directory where the browser binary and assets are installed |
| `VNC_DISPLAY` | `:99` | X display number the browser and Xvfb run on |
| `VNC_PORT` | `5900` | Port x11vnc serves the screen on |
| `VNC_PASSWORD` | `headless` | VNC access password, refreshed on each container boot |
| `BIND_ADDR` | `0.0.0.0` | Address the web UI and CDP proxy listen on inside the container |
| `WEB_PORT` | `8000` | Port for the web UI and WebSocket VNC bridge |
| `VNC_HOST` | `127.0.0.1` | Internal address the web UI bridge uses to reach x11vnc |
| `CDP_PORT` | `9222` | Port for the CDP proxy |
| `BROWSER_CDP_URL` | `http://127.0.0.1:9224/json/version` | Internal Brave Origin DevTools endpoint used to resolve the WebSocket URL |
| `NOVNC_DIR` | `/usr/share/novnc` | System path where noVNC client assets reside |
| `RUST_LOG` | `headless_brave_web=info,warn` | Tracing filter for the Rust supervisor service |

## Connecting via CDP (AI Agents)

The Chrome DevTools Protocol endpoint is exposed on port `9222`.

### opencode

Add to `~/.config/opencode/opencode.jsonc`:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "browser": {
      "type": "local",
      "command": ["npx", "-y", "@playwright/mcp", "--cdp-endpoint", "ws://127.0.0.1:9222"],
      "enabled": true
    }
  }
}
```

### Claude Code

Add to `~/.claude/settings.json`:

```json
{
  "mcpServers": {
    "browser": {
      "command": "npx",
      "args": ["-y", "@playwright/mcp", "--cdp-endpoint", "ws://127.0.0.1:9222"]
    }
  }
}
```

### Zed

Add to `~/.config/zed/settings.json`:

```json
{
  "browser": {
    "command": "npx",
    "args": ["-y", "@playwright/mcp", "--cdp-endpoint", "ws://127.0.0.1:9222"],
    "env": {}
  }
}
```

### Playwright (Direct)

```javascript
const { chromium } = require('playwright');
const browser = await chromium.connectOverCDP('ws://127.0.0.1:9222/');
const [page] = browser.contexts()[0].pages();
await page.goto('https://example.com');
```

## Storage & Persistence

The container uses two distinct volumes:

- `/data`: Holds the browser profile (cookies, saved logins, history, extensions, local storage). Persists across container rebuilds and restarts.
- `/opt`: Holds the extracted Brave Origin package. Installed automatically on first run and updated in place.

### Resetting State

To discard both profile state and the installed browser:

```bash
docker compose down -v
```

### Host Bind Mounts

To persist profile data to a host directory instead of a named Docker volume, use a bind mount:

```yaml
    volumes:
      - /srv/headless-brave:/data
```

The container runs as unprivileged user `headless` (UID 1000). Ensure the host directory is owned by UID 1000:

```bash
install -d -o 1000 -g 1000 /srv/headless-brave
```

Named volumes defined in `compose.yaml` inherit directory permissions automatically on creation.

## Security & Network Isolation

- **No authentication:** Ports `8000` (Web UI) and `9222` (CDP) provide unrestricted access to anyone who can reach them. Port `5900` requires a password, but the web UI retrieves it via `/api/config`.
- **Loopback binding:** The default `compose.yaml` binds all published ports to `127.0.0.1` on the host. Do not expose these ports on public interfaces without an authenticating reverse proxy.
- **Unprivileged execution:** The container runs as UID 1000 with Chromium's user-namespace sandbox active. It does not require root privileges or `--privileged`.
