# Headless Brave

Run Brave in a container with a VNC desktop, a web-based VNC client, and
Chrome DevTools Protocol access — for AI agents and browser automation.

The browser belongs to whoever is driving it over CDP. A desktop is available
for anyone who wants to look, and as many of them as like: watching never
disturbs the driver, and no viewer can take the screen away from the agent or
from another viewer.

## What it does

- Runs Brave on a virtual screen, started once at boot and kept alive, so the
  browser is there whether or not anyone is watching
- Shares that screen over VNC on port `5900`, to any number of clients
- Serves the same screen to a browser on port `80`, through noVNC
- Exposes CDP on port `9222` for Playwright, Puppeteer and friends
- Runs the browser as an unprivileged user, with its own sandbox on, so a
  compromised page is confined to the browser's renderer rather than the
  container
- Ships a Rust service (`headless-brave-web`) that serves the web UI, the
  noVNC client and both WebSocket bridges; there is no Python anywhere in the
  image

## Quick start

```bash
docker compose up -d --build
```

The ports are published on the IPv4 loopback only. `localhost` resolves to
`::1` first on many systems, and podman does not forward IPv6 loopback, so
spell out `127.0.0.1` — for the web UI, for VNC clients, and in the MCP
configuration below.

### Connect via the web UI

```
http://127.0.0.1:8000/
```

The page is nothing but the desktop: it attaches on load, and there is nothing
to press. Click to type, and it reconnects on its own if the screen goes away.

### Connect via VNC

```
Host: 127.0.0.1
Port: 5900
Password: headless
```

Any VNC client will do. The screen is shared, so you can watch alongside
somebody else, and a viewer leaving never disturbs the browser. Typing is
possible, but nothing a viewer does reaches the agent's session any differently
than the agent's own work does.

### Connect via CDP (for AI agents)

<details>
<summary><b>opencode</b></summary>

`~/.config/opencode/opencode.jsonc`:
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

</details>

<details>
<summary><b>Claude Code</b></summary>

`~/.claude/settings.json`:
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

</details>

<details>
<summary><b>Zed</b></summary>

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
</details>

Or connect directly with Playwright:

```javascript
const { chromium } = require('playwright');
const browser = await chromium.connectOverCDP('ws://127.0.0.1:9222/');
const [page] = browser.contexts()[0].pages();
await page.goto('https://example.com');
```

## Keeping data

The browser's profile — cookies, logins, history, extensions, local storage —
lives on a named volume at `/data`, so it survives a rebuild, a recreate and a
plain restart. Anything you log into on the web UI is still logged in next
time.

The browser itself lives on a second volume, at `/opt`, and is installed the
first time the container starts. Only the shared libraries it links against are
in the image, so a new Brave release costs a restart rather than a rebuild:

```bash
BRAVE_UPGRADE=1 docker compose up -d    # ask apt for a newer Brave
```

`BRAVE_UPGRADE` is off by default because it downloads on every start. The
first boot needs the network; after that a restart does not.

```bash
docker compose down -v   # throws the profile and the browser away
```

To keep the profile somewhere on the host instead, replace the volume with a
bind mount and point `BRAVE_PROFILE` at it:

```yaml
    volumes:
      - /srv/headless-brave:/data
```

The profile belongs to the unprivileged user the browser runs as, and the
service hands the whole tree over on every start — which is what makes a
profile written by an earlier container that ran as root usable rather than
half-broken. A bind mount needs no pre-chowning.

One thing to know: a persisted profile keeps Chromium's single-instance lock,
which names a process that no longer exists after a stop. The service clears
it before launching the browser — without that, Brave refuses to start and the
container comes up with no browser at all. For the same reason the browser is
told not to offer session restore: every stop looks like a crash to it, and the
"Restore pages?" bubble would sit on the shared desktop covering whatever the
agent was doing.

## Customization

| Variable | Default | Description |
|---|---|---|
| `RESOLUTION` | `1920x1080` | Size of the virtual screen |
| `BRAVE_PROFILE` | `/data/profile` | Where the browser keeps its profile; the compose file mounts a volume here |
| `BRAVE_ROOT` | `/opt/brave.com` | Where the browser is installed; the compose file mounts a volume here |
| `BRAVE_UPGRADE` | `0` | `1` asks apt for a newer Brave on every start |
| `VNC_DISPLAY` | `:99` | X display the browser runs on |
| `VNC_PORT` | `5900` | Port x11vnc serves the screen on |
| `VNC_PASSWORD` | `headless` | VNC password, stored on every boot |
| `WEB_PORT` | `80` | Port for the web UI and its bridge |
| `CDP_PORT` | `9222` | Port for the CDP proxy |
| `BROWSER_CDP_URL` | `http://127.0.0.1:9224/json/version` | Brave's DevTools endpoint, used to find its WebSocket |
| `NOVNC_DIR` | `/usr/share/novnc` | Where the noVNC client is installed |
| `RUST_LOG` | `headless_brave_web=info,warn` | Log filter for `headless-brave-web` |

Ports `80` and `9222` do not authenticate the *caller* at all: anyone who can
reach them can watch the screen, type into it, or drive the browser. Port
`5900` asks for a password, but the password is handed to the web UI, which
hands it to anyone who can reach port 80. The compose file therefore publishes
all three on loopback; if you need them off-host, put an authenticating reverse
proxy in front.

## How it works

```mermaid
flowchart TB
    subgraph outside["outside the container"]
        person["person's browser"]
        vncclient["VNC client"]
        agent["automation<br/>Playwright · MCP"]
    end

    subgraph container["headless-brave container"]
        entry["headless-brave-web<br/>PID 1: supervises all of it"]
        subgraph service["headless-brave-web · Rust"]
            webui["web UI :80<br/>noVNC client"]
            sockify["WebSocket bridge<br/>/websockify"]
            cdpproxy["CDP proxy :9222"]
            novnc["noVNC assets<br/>/novnc"]
        end
        xvfb["Xvfb :99<br/>the shared screen"]
        wm["fluxbox"]
        brave["Brave<br/>CDP :9224"]
        vnc["x11vnc :5900"]
    end

    person -- "HTTP" --> webui
    webui --> novnc
    webui -- "WebSocket" --> sockify
    sockify -- "VNC" --> vnc
    vncclient -- "VNC" --> vnc
    agent -- "WebSocket" --> cdpproxy
    cdpproxy -- "DevTools WS" --> brave
    brave -- "profile" --> profile
    vnc -- "screen" --> xvfb
    xvfb --- wm
    wm --- brave
    entry -. "starts" .-> xvfb
    entry -. "starts" .-> vnc
    entry -. "starts" .-> service
```

The screen is started at boot, not by anyone looking at it. That is
what keeps the browser — and therefore CDP — alive whether or not a viewer is
attached.

The web UI is a thin page around noVNC's client library, which the distribution
installs at `/usr/share/novnc` and the service serves from there. noVNC speaks
WebSocket, x11vnc speaks TCP, and the `/websockify` endpoint in between is a
plain byte relay — a VNC session is binary from the first challenge on, so
nothing on that path may go through a string.

The page is sent its connection settings by the container, not asked for them,
so there is nothing on screen to fill in.

## Files

| File | Purpose |
|---|---|
| `Dockerfile` | Builds the Rust service, and the runtime image around Brave's dependencies and x11vnc |
| `src/` | `headless-brave-web` — the whole container: the browser install, the screen, the window manager, x11vnc, the web UI, the VNC bridge, the CDP proxy and the noVNC assets |
| `web/` | The page and its script |
| `.devcontainer/` | Development container |
| `docker-compose.yml` | Port mappings, profile volume and defaults |

Working on it is covered in [docs/development.md](docs/development.md): the dev
container and the checks.

## License

MIT. Uses [noVNC](https://novnc.com), MPL-2.0, served from the distribution's
package rather than vendored.
