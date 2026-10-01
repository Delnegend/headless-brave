# Development

How to work on headless-brave: the dev container, the checks, and the parts of
the image that are easy to break by accident.

For what the thing does and how to run it, see the [README](../README.md).

## Checks

The dev container (`.devcontainer/`) has Rust stable, clippy, rustfmt, mold and
podman, so the image can be built and run from inside it.

```bash
# the CI gate, and what to run before pushing
just check

# the same thing by hand
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test

# the whole image
podman build -t headless-brave:local .
podman run --rm --shm-size=2g \
  -p 127.0.0.1:8000:8000 -p 127.0.0.1:5900:5900 -p 127.0.0.1:9222:9222 \
  --security-opt label=disable headless-brave:local
```

`--shm-size` is not optional: Chromium is unhappy on Docker's 64 MB default.

### Lints

`[lints.clippy]` in `Cargo.toml` turns on `pedantic` and `nursery` and denies
everything that can abort at runtime — `unwrap_used`, `expect_used`,
`indexing_slicing`, `arithmetic_side_effects`, `panic`, `unreachable`, `todo`,
`string_slice`, `exit`, `as_conversions`. The set follows
[this post](https://www.namtao.com/rust/); `clippy.toml` allows the assertion
lints inside tests.

A WebSocket bridge in a container has no user to report a panic to: the task
dies, the desktop keeps running with no way in, and the only trace is one line
in `docker logs`. Adopting the set found three such deaths — a close reason cut
on a byte count that can land inside a character, a percent-decoder indexing
past the end of its input, and a static path that could climb out of the noVNC
tree — all now checked, with tests for the boundaries.

`nursery` churns between Rust releases, so a toolchain bump surfaces new
findings. `rust-toolchain.toml` pins the version the image is built with.

## One binary is the container

`headless-brave-web` is PID 1. It installs the browser if the volume is empty,
clears the locks a killed process left behind, starts Xvfb, fluxbox, the browser
and x11vnc, serves the web UI, and takes the whole container down if any of them
exits so the restart policy can start a fresh one. No entrypoint script, and no
second place the configuration is parsed.

Nothing runs as root: the image creates a `headless` user and the service is it,
so the install, the screen, the browser and both servers share one unprivileged
owner. The volumes are created by the image for the same reason — a volume
inherits the ownership of the directory it is mounted over.

`SIGTERM` is a clean path: children are asked to stop, given five seconds, then
killed, which is long enough for the browser to close its profile.

## What comes from where

- **noVNC** is the distribution's `novnc` package at `/usr/share/novnc`, served
  from there. Being packaged is why there is no `build.rs` fetching a pinned
  copy.
- **Brave** is installed at start onto a volume at `/opt`, so a new release
  costs a restart rather than a rebuild; the image keeps the shared libraries
  and drops the payload, which is the part that moves. A ticker asks the
  repository every six hours and installs whatever is newer, because Brave ships
  security releases weekly and this is the container nobody remembers to update.
  The install is `apt-get download` plus `dpkg-deb --extract` — no root, and no
  dpkg database to reinstall into — with apt pointed at an index of its own
  under the temporary directory, since the image's is not ours to write. The
  package is unpacked and moved *inside* the volume, because a rename cannot
  cross filesystems and `/tmp` is not on it. The version unpacked is recorded in
  `/opt/brave.com/VERSION`, which is what the ticker compares against; a
  repository it could not ask is not an upgrade, or a network outage would
  reinstall on every tick.
- **The browser** runs as `headless`, because Chromium refuses to start as root
  without `--no-sandbox` and the sandbox is worth keeping for something browsing
  the open web. Only the user-namespace sandbox is available, so the install
  deletes the SUID helper the package ships: an unprivileged `dpkg-deb --extract`
  cannot make it root-owned, and Chromium selects it on being present and
  executable, then aborts fatally rather than falling back. `check_sandbox`
  confirms namespaces exist before the browser starts, so a host without them is
  told in one line instead of a stack trace.
- **x11vnc** serves the screen. `-shared` lets several people watch at once,
  `-forever` keeps serving after the last one leaves, and the browser is driven
  over CDP rather than by whoever is looking.
- **Xvfb** provides the screen. A restart that killed the previous one leaves
  `/tmp/.X99-lock` behind and the next Xvfb refuses to start, so the service
  clears it.

## Layout

| Path | What lives there |
|---|---|
| `src/main.rs` | entry point: tracing, config, then the supervisor |
| `src/config.rs` | every environment variable, parsed once |
| `src/supervise.rs` | PID 1 — installs the browser, starts everything, stops it if one part dies |
| `src/web.rs` | routes, and the static handler for the noVNC tree |
| `src/bridge.rs` | the two WebSocket relays |
| `src/cdp.rs` | finding the browser's DevTools WebSocket |
| `src/assets.rs` | the embedded page and script |
| `web/` | the page and its script, embedded from here |
| `Dockerfile` | the image |
| `compose.yaml` | ports, volumes, log driver |
| `justfile` | `just check`, which is the CI gate |
| `scripts/smoke.sh` | functional check against a running container |
| `clippy.toml` | test allowances for the strict lint set |

## Testing notes

`cargo test` covers the decoders and the file handler: the percent-decoder, the
close-reason truncation, and refusing a noVNC path that climbs out of its tree.
These are where a malformed input from a peer could take the service down.

Everything else needs a real container, and `scripts/smoke.sh` asks the running
one — the web UI, the noVNC client, the browser process, the CDP bridge, and the
two invariants worth protecting: nothing runs as root, and the sandbox is on.

```bash
./scripts/smoke.sh
CONTAINER=brave-headless CDP_PORT=9222 ./scripts/smoke.sh
```

It uses docker or podman, whichever the host has. CI runs part of this
automatically, but not all of it: GitHub-hosted runners forbid unprivileged
user namespaces, so the full check needs a normal Linux host.

One thing nothing can automate: open the page twice and confirm both viewers stay
connected and neither bounces the other.
