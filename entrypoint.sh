#!/bin/bash
# Boots the container's moving parts and supervises them:
#
#   Xvfb            a virtual screen for the browser, with no window manager
#   fluxbox         the window manager on that screen
#   x11vnc          shares the screen over VNC, to any number of viewers
#   headless-brave-web  the web UI on 80, and the CDP proxy on 9222
#
# The browser is started here, once, and is not tied to anyone watching it: an
# agent drives it over CDP whenever it likes, and a viewer attaches to the
# same screen whenever they like, without either disturbing the other.
set -euo pipefail

log() { printf '%s headless-brave %s\n' "$(date -u +%FT%TZ)" "$*" >&2; }

resolution="${RESOLUTION:-1920x1080}"
display="${VNC_DISPLAY:-:99}"
vnc_port="${VNC_PORT:-5900}"
password="${VNC_PASSWORD:-headless}"
web_port="${WEB_PORT:-80}"
cdp_port="${CDP_PORT:-9222}"
profile="${BRAVE_PROFILE:-/data/profile}"
brave_cdp_port="${BROWSER_CDP_PORT:-9224}"
brave_root="${BRAVE_ROOT:-/opt/brave.com}"
brave_binary="$brave_root/brave/brave"

if [[ ! $resolution =~ ^[0-9]+x[0-9]+$ ]]; then
  log "RESOLUTION must look like 1920x1080, got '$resolution'"
  exit 1
fi
if [[ ! $display =~ ^:[0-9]+$ ]]; then
  log "VNC_DISPLAY must look like :99, got '$display'"
  exit 1
fi
if [[ -z $password ]]; then
  log "VNC_PASSWORD must not be empty"
  exit 1
fi
if [[ $profile != /* ]]; then
  log "BRAVE_PROFILE must be an absolute path, got '$profile'"
  exit 1
fi
# The install creates the directory tree, so check the mount point it hangs
# from rather than the directory itself.
brave_mount="${brave_root%/*}"
if [[ $brave_root != /* || $brave_mount == "$brave_root" || ! -d $brave_mount ]]; then
  log "BRAVE_ROOT must sit under an existing directory, got '$brave_root'"
  exit 1
fi

mkdir -p "$profile" /run/x11vnc /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix

# A restart that killed the previous Xvfb leaves its display locked, and the
# next one refuses to start over a lock nobody is holding.
rm -f "/tmp/.X${display#:}-lock" "/tmp/.X11-unix/X${display#:}"

# x11vnc's -storepasswd writes to a file it will not create for itself.
touch /run/x11vnc/passwd

# The browser lives on a volume so that a new release costs a restart rather
# than a rebuild. Its shared libraries are already in the image, so this is one
# package: on the first boot only.
install_brave() {
  log "installing Brave into ${brave_root}"
  apt-get update -qq
  # --reinstall because the image records the package as installed while its
  # payload has been dropped and lives on the volume instead.
  apt-get install -y -qq --no-install-recommends --reinstall brave-browser
  rm -rf /var/lib/apt/lists/*
}

if [[ ! -x $brave_binary ]]; then
  install_brave
elif [[ ${BRAVE_UPGRADE:-0} == 1 ]]; then
  # Ask apt whether there is anything newer, and take it if so. Off by default
  # because it costs a download on every start.
  log "checking for a newer Brave"
  apt-get update -qq
  apt-get install -y -qq --no-install-recommends brave-browser
  rm -rf /var/lib/apt/lists/*
fi

if [[ ! -x $brave_binary ]]; then
  log "Brave is not installed at $brave_binary"
  exit 1
fi

children=()
names=()
shutdown() {
  log "stopping"
  kill "${children[@]}" 2>/dev/null || true
}
trap shutdown TERM INT

log "starting Xvfb on $display at $resolution"
Xvfb "$display" -screen 0 "${resolution}x24" -nolisten tcp -noreset &
children+=($!)
names+=(Xvfb)

# Wait for the screen rather than sleeping: fluxbox and the browser both fail
# in confusing ways if they start before it exists.
for _ in {1..100}; do
  if [[ -e /tmp/.X11-unix/X${display#:} ]]; then
    break
  fi
  sleep 0.1
done
if [[ ! -e /tmp/.X11-unix/X${display#:} ]]; then
  log "Xvfb did not come up on $display"
  exit 1
fi

log "starting fluxbox"
DISPLAY="$display" fluxbox >/dev/null 2>&1 &
children+=($!)
names+=(fluxbox)

# A profile that outlives the container keeps Chromium's single-instance lock,
# which names a process that no longer exists. Left alone, Brave refuses to
# start and the container comes up with no browser at all.
rm -f "$profile"/Singleton{Lock,Cookie,Socket}

width="${resolution%x*}"
height="${resolution#*x}"

log "starting Brave"
# The container is stopped abruptly, so a profile that survives it always looks
# like a crash to the browser. Without this, a "Restore pages?" bubble is
# waiting on the shared desktop every time, covering whatever it was doing.
DISPLAY="$display" "$brave_binary" \
  --no-sandbox \
  --disable-gpu \
  --no-first-run \
  --no-default-browser-check \
  --disable-features=Translate \
  --hide-crash-restore-bubble \
  --window-size="${width},${height}" \
  --user-data-dir="$profile" \
  --remote-debugging-port="$brave_cdp_port" \
  about:blank >/tmp/brave.log 2>&1 &
children+=($!)
names+=(Brave)

# x11vnc only writes a password file on its own; the server then just reads it.
x11vnc -quiet -storepasswd "$password" /run/x11vnc/passwd

# -shared: every viewer sees the same screen and none of them owns it.
# -forever: keep serving after the last viewer disconnects, which is the whole
# point — the browser is driven over CDP, not by whoever happens to be looking.
log "starting x11vnc on $vnc_port"
# No -localhost here: the compose file already publishes this on the loopback,
# and a published port is forwarded to the container's own address, which
# x11vnc would refuse if it were told to listen on loopback only.
x11vnc -display "$display" -rfbport "$vnc_port" \
  -rfbauth /run/x11vnc/passwd \
  -forever -shared -repeat -noxdamage \
  >/tmp/x11vnc.log 2>&1 &
children+=($!)
names+=(x11vnc)

log "starting headless-brave-web on $web_port (CDP on $cdp_port)"
VNC_PORT="$vnc_port" WEB_PORT="$web_port" CDP_PORT="$cdp_port" \
  VNC_PASSWORD="$password" headless-brave-web &
children+=($!)
names+=(headless-brave-web)

# Any of the above dying takes the container down, so the supervisor restarts
# the whole thing rather than leaving a half-working browser behind. The
# `wait` sits in a condition because a child exiting non-zero must reach the
# logging below, not trip `set -e` on the way.
# `wait -n` has to sit in a condition: a child exiting non-zero is the normal
# case here, and under `set -e` it would abort before the logging below.
status=0
wait -n || status=$?
for index in "${!children[@]}"; do
  if ! kill -0 "${children[$index]}" 2>/dev/null; then
    log "${names[$index]} exited"
  fi
done
log "a supervised process exited (status $status)"
shutdown
exit 1
