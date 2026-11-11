# Deployment & Operations

## Run with Podman

```bash
# Pull published image
podman pull ghcr.io/delnegend/headless-brave:latest

# Run container with virtual display and DevTools ports
podman run -d --name headless-brave --shm-size=2g \
  -p 127.0.0.1:8000:8000 -p 127.0.0.1:5900:5900 -p 127.0.0.1:9222:9222 \
  -v headless-brave-data:/data -v headless-brave-opt:/opt \
  ghcr.io/delnegend/headless-brave:latest
```

Visit `http://127.0.0.1:8000` to view the desktop, or connect agents via `ws://127.0.0.1:9222`.

## Run with Docker Compose

A pre-configured `compose.yaml` is provided:

```bash
docker compose up -d
```

## Logs & Verification

Inspect supervisor and process output:

```bash
docker compose logs -f
```

Run manual health checks on the running container:

```bash
./scripts/smoke.sh
```

## Upgrades

The container checks and updates the browser binary in `/opt` automatically every 6 hours without container restarts. To update the underlying container image:

```bash
docker compose pull
docker compose up -d
```
