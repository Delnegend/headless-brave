#!/usr/bin/env bash
# Checks a running headless-brave container the way a user would use it.
#
# CI cannot do this: GitHub-hosted runners forbid unprivileged user namespaces,
# and this container's browser needs one, because it is installed unprivileged
# and so cannot use Chromium's root-owned SUID sandbox helper. Run it on a host
# that allows them - a normal Linux desktop or server, which is where this
# thing is meant to run.
#
#   ./scripts/smoke.sh [web-port]
#
# Exits non-zero if anything is wrong, and says which.

set -u -o pipefail

port="${1:-8000}"
cdp="${CDP_PORT:-9222}"
base="http://127.0.0.1:${port}"
container="${CONTAINER:-headless-brave}"

# Podman and Docker are the same interface for everything used here, and this
# project is run under both, so use whichever this host has.
if [ -n "${RUNTIME:-}" ]; then
  :
elif command -v docker >/dev/null 2>&1; then
  RUNTIME=docker
elif command -v podman >/dev/null 2>&1; then
  RUNTIME=podman
else
  echo "smoke: neither docker nor podman found; set RUNTIME" >&2
  exit 2
fi

failures=0

pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
check() { if [ "$1" = "0" ]; then pass "$2"; else fail "$2"; fi; }

echo "smoke: ${container} on ${base} (via ${RUNTIME})"

# 1. The web service answers. This is what a user notices first, and on a first
#    boot it is also what takes longest: the browser has to be downloaded.
for _ in $(seq 1 60); do
  curl -fsS "${base}/healthz" >/dev/null 2>&1 && break
  sleep 5
done
curl -fsS "${base}/healthz" >/dev/null 2>&1
check $? "the web service answers"

# 2. The web UI, and the noVNC client it serves for VNC viewers.
curl -fsS "${base}/" | grep -qi brave
check $? "the web UI is served"
curl -fsS "${base}/novnc/core/rfb.js" >/dev/null 2>&1
check $? "a noVNC asset is served"

# 3. The browser installed, unprivileged, and recorded what it unpacked.
version=$("$RUNTIME" exec "$container" cat /opt/brave.com/VERSION 2>/dev/null)
[ -n "$version" ]
check $? "the browser is installed (${version:-unknown})"

# 4. It is running.
"$RUNTIME" exec "$container" pgrep -f 'brave-origin/brave --disable-gpu' >/dev/null 2>&1
check $? "the browser process is running"

# 5. The sandbox is on. A browser that cannot sandbox will not start as this
#    user at all, so getting here is most of it; the flag is the part that
#    could be reintroduced quietly.
flags=$("$RUNTIME" exec "$container" sh -c \
  "tr '\\0' '\\n' < /proc/\$(pgrep -f remote-debugging-port | head -1)/cmdline | grep -c '^--no-sandbox\$'" 2>/dev/null)
[ "$flags" = "0" ]
check $? "the browser's sandbox is on"

# 6. The SUID helper is gone, so Chromium uses namespaces rather than selecting
#    a helper it cannot accept.
"$RUNTIME" exec "$container" sh -c 'test ! -e /opt/brave.com/brave-origin/chrome-sandbox'
check $? "the unusable SUID helper is absent"

# 7. The invariant the whole container is built around.
[ "$("$RUNTIME" exec "$container" sh -c "ps -eo user= | grep -c '^root'")" = "0" ]
check $? "nothing runs as root"
[ "$("$RUNTIME" exec "$container" ps -o user= -p 1)" = "headless" ]
check $? "pid 1 is the unprivileged user"

# 8. The CDP bridge. It speaks WebSocket, so this request cannot succeed - it
#    can only be refused, and a refusal means the bridge never bound.
bridge=$(curl -sS -o /dev/null -w '%{http_code}' --max-time 10 "http://127.0.0.1:${cdp}/" 2>/dev/null || true)
[ -n "$bridge" ]
check $? "the CDP bridge is listening on ${cdp} (answered ${bridge:-nothing})"

if [ "$failures" -eq 0 ]; then
  echo "smoke: everything passed"
else
  echo "smoke: ${failures} check(s) failed" >&2
  exit 1
fi
