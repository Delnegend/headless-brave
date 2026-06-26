#!/bin/bash
set -e

RES="${RESOLUTION:-1920x1080}"

export DISPLAY=:99

Xvfb :99 -screen 0 "${RES}"x24 &
sleep 1

fluxbox &

mkdir -p /root/.vnc
x11vnc -storepasswd "${VNC_PASSWORD:-headless}" /root/.vnc/passwd
x11vnc -display :99 -forever -usepw -rfbport 5900 &
sleep 1

win_size=$(echo "${RES}" | tr x ,)
/usr/bin/brave-browser --no-sandbox --disable-gpu --window-size="${win_size}" \
  --remote-debugging-port=9224 \
  2>/tmp/brave.log &

python3 /cdp-proxy.py &

wait
