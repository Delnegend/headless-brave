# Development

How to work on headless-brave: the dev container, the checks, and the parts of
the image that are easy to break by accident.

For what the thing does and how to run it, see the [README](../README.md).

## Dev container

The repository is also a dev container: Rust stable with rust-analyzer, clippy,
rustfmt, the musl target and mold, plus podman so the image can be built and
run from inside it with the commands below.

```
.devcontainer/
├── Dockerfile          debian:13-slim, rust stable + mold, podman
├── devcontainer.json   user, cargo and container-storage volumes
└── postinstall.sh      warms the cargo cache
```

## Checks

```bash
# the web service on its own, for an edit-compile loop
cargo build
cargo clippy --all-targets
cargo fmt --check
cargo test

# the whole image, built and run with podman from inside the dev container
podman build -t headless-brave:local .
podman run --rm -p 127.0.0.1:80:80 -p 127.0.0.1:5900:5900 -p 127.0.0.1:9222:9222 \
  --security-opt label=disable headless-brave:local
```

### Lints

`[lints.clippy]` in `Cargo.toml` turns on `pedantic` and `nursery` and denies
everything that can abort at runtime: `unwrap_used`, `expect_used`,
`indexing_slicing`, `arithmetic_side_effects`, `panic`, `unreachable`,
`todo`, `string_slice`, `exit` and `as_conversions`. The set follows
[this post](https://www.namtao.com/rust/); `clippy.toml` allows the assertion
lints inside tests, where a failure is a bug in the test rather than in the
code under test.

The point is not the pedantic ones. A WebSocket bridge in a container has no
user to report a panic to: the task dies, the desktop keeps running with no way
in, and the only trace is one line in `docker logs`. Adopting the set found
three ways this binary could have died that way — a close reason cut on a byte
count that can land inside a character, a percent-decoder indexing past the end
of its input, and a static-file path that could climb out of the noVNC tree —
all now checked, with tests for the boundaries.

`nursery` churns between Rust releases, so a toolchain bump can surface new
findings. `rust-toolchain.toml` pins the version the image is built with.

## One binary is the container

`headless-brave-web` is PID 1. It installs the browser if the volume is empty,
clears the locks a killed process left behind, starts Xvfb, fluxbox, the browser
and x11vnc, serves the web UI itself, and if any of them exits it takes the
whole container down so the restart policy can start a fresh one. There is no
entrypoint script, and the configuration is parsed in one place rather than once
in the script and once in the code.

Nothing in the container runs as root. The image creates a `headless` user and
the service is it, so the install, the screen, the browser and both servers all
belong to the same unprivileged owner and nothing has to be given away at
runtime. The volumes it works on are created by the image for the same reason:
a volume inherits the ownership of the directory it is mounted over.

`SIGTERM` (a `docker stop`) is a clean path: children are asked to stop, given
five seconds, and killed if they have not — long enough for the browser to close
its profile, which is the difference between a clean shutdown and a "didn't shut
down correctly" on the next start.

## What comes from where

There is no compiler in the runtime image: everything arrives as Debian
packages, and the only thing built is the Rust service.

- **noVNC** is the distribution's `novnc` package, installed at
  `/usr/share/novnc` and served from there. It is the client half of the web
  UI, and it is the reason there is no `build.rs` fetching a pinned copy: the
  package is versioned, patched and upgraded by the distribution.
- **Brave** is installed at start onto a volume at `/opt`, so a new
  release costs a restart instead of a rebuild. The image keeps the shared
  libraries it links against and drops the payload again, which is the part
  that moves. The first boot needs the network; later ones do not.
  Keeping it current is a ticker rather than a switch: it asks the repository
  every six hours and installs whatever is newer. Brave ships security
  releases weekly at best, and this container is exactly the kind that nobody
  remembers to update.
  The install is `apt-get download` plus `dpkg-deb --extract` rather than
  `apt-get install`, because there is no root here and no dpkg database in the
  container to reinstall into. apt is pointed at an index under the temporary
  directory, since the one in the image is not ours to write, and the package
  is unpacked and moved *inside* the volume: a rename cannot cross filesystems,
  and `/tmp` is not on the volume. The version it unpacked is recorded in
  `/opt/brave.com/VERSION`, which is what the ticker compares against. A
  repository it could not ask is not treated as an upgrade: otherwise a
  network outage would put a reinstall on every tick.
- **The browser** runs as the unprivileged `headless` user the image creates,
  because Chromium refuses to start as root unless `--no-sandbox` is passed,
  and the sandbox is worth keeping for something browsing the open web. The
  flag is not passed: both sandbox backends are available here — user
  namespaces and the setuid `chrome-sandbox` that ships with the package — so
  it does not depend on either alone.
- **x11vnc** serves the screen. Two of its flags matter and are easy to undo by
  accident. `-shared` is what lets several people watch at once, and `-forever`
  is what keeps serving after the last one leaves — the browser is driven over
  CDP, not by whoever is looking.
- **Xvfb** provides the screen. A restart that killed the previous one leaves
  `/tmp/.X99-lock` behind and the next Xvfb refuses to start, so the service
  clears it.

## Layout

| Path | What lives there |
|---|---|
| `src/` | `headless-brave-web` — the web UI, the VNC bridge, the CDP proxy |
| `src/bridge.rs` | the two WebSocket relays |
| `src/web.rs` | routes, and the static file handler for the noVNC tree |
| `src/cdp.rs` | finding the browser's DevTools WebSocket |
| `web/` | the page and its script, embedded into the binary |
| `src/supervise.rs` | PID 1: installs the browser, starts everything, takes the container down if one stops |
| `clippy.toml` | test allowances for the strict lint set |

## Testing notes

`cargo test` covers the decoders and the file handler: the percent-decoder,
the close-reason truncation, and refusing a noVNC path that climbs out of its
tree. These are the places a malformed input from a peer could take the service
down, and the ones the lint set pushed into the open.

For the parts that need a real container, ask the running thing:

```bash
# is the service up? (the compose file publishes the web UI on 8000)
curl -sS http://127.0.0.1:8000/healthz

# did everything start, and did anything complain?
podman logs brave-headless

# is the VNC screen answering? its port is container-internal
podman exec brave-headless timeout 2 head -c 12 < /dev/tcp/127.0.0.1/5900

# is the browser answering CDP? also container-internal
podman exec brave-headless wget -qO- http://127.0.0.1:9224/json/version
```

The web UI is deliberately hard to check by hand — there is nothing to click.
Open it and look: it attaches on load, and a desktop appears.

The thing worth testing properly is two viewers at once, since that is the
reason this is VNC and not RDP: open the page twice and confirm both stay
connected, neither bouncing the other.

