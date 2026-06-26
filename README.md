# Headless Brave

Run Brave browser in a Docker container with VNC and Chrome DevTools Protocol (CDP) access, perfect for AI agents and browser automation.

## What it does

- Launches Brave inside a Docker container with a virtual display (Xvfb)
- Provides VNC access on port `5900` for visual debugging
- Exposes CDP on port `9222` for programmatic control via Playwright/Puppeteer
- Includes a WebSocket proxy so tools can connect to `ws://localhost:9222/` without worrying about CDP's dynamic endpoint path

## Who it's for

- **AI agents** needing a real browser for web tasks (navigate, click, type, screenshot)
- **Developers** wanting a disposable, containerized browser for testing
- **Automation pipelines** that need a consistent browser environment

## Quick start

```bash
docker compose up -d --build
```

### Connect via VNC

```
Host: localhost
Port: 5900
Password: headless
```

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
      "command": ["npx", "-y", "@playwright/mcp", "--cdp-endpoint", "ws://localhost:9222"],
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
      "args": ["-y", "@playwright/mcp", "--cdp-endpoint", "ws://localhost:9222"]
    }
  }
}
```

</details>

<details>
<summary><b>Zed</b></summary>

`~/.config/zed/settings.json`:
```json
{
  "mcp": {
    "browser": {
      "command": ["npx", "-y", "@playwright/mcp", "--cdp-endpoint", "ws://localhost:9222"]
    }
  }
}
```

</details>

Or connect directly with Playwright:

```javascript
const { chromium } = require('playwright');
const browser = await chromium.connectOverCDP('ws://localhost:9222/');
const [page] = browser.contexts()[0].pages();
await page.goto('https://example.com');
```

## Customization

| Variable | Default | Description |
|---|---|---|
| `RESOLUTION` | `1920x1080` | Display resolution |
| `VNC_PASSWORD` | `headless` | VNC access password |

## How it works

```
Brave (--remote-debugging-port=9224)
  │
  ├── Xvfb (virtual framebuffer) ← VNC (x11vnc :5900)
  │
  └── CDP (:9224) → cdp-proxy.py (:9222)
                       │
                       └── ws://localhost:9222/ ← Playwright / opencode tools
```

## Files

| File | Purpose |
|---|---|
| `Dockerfile` | Ubuntu + Xvfb + x11vnc + Brave + CDP proxy |
| `entrypoint.sh` | Starts Xvfb → fluxbox → x11vnc → Brave → CDP proxy |
| `cdp-proxy.py` | WebSocket proxy that bridges `ws://localhost:9222/` to Brave's dynamic CDP endpoint |
| `docker-compose.yml` | Container config with port mappings and shared memory |

## License

MIT
