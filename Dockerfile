FROM ubuntu:22.04

ENV DEBIAN_FRONTEND=noninteractive
ENV RESOLUTION=1920x1080
ENV DISPLAY=:99

RUN apt-get update && apt-get install -y \
    wget \
    gnupg \
    xvfb \
    x11vnc \
    fluxbox \
    socat \
    python3-pip \
    && rm -rf /var/lib/apt/lists/*

RUN wget -q -O - https://brave-browser-apt-release.s3.brave.com/brave-browser-archive-keyring.gpg \
    | gpg --dearmor -o /usr/share/keyrings/brave-browser-archive-keyring.gpg \
    && echo "deb [arch=amd64 signed-by=/usr/share/keyrings/brave-browser-archive-keyring.gpg] https://brave-browser-apt-release.s3.brave.com/ stable main" \
    > /etc/apt/sources.list.d/brave-browser-release.list \
    && apt-get update && apt-get install -y brave-browser \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /root/.fluxbox && /usr/bin/fluxbox-update_configs >/dev/null 2>&1

RUN pip3 install --upgrade websockets 2>&1 | tail -3

COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

COPY cdp-proxy.py /cdp-proxy.py

EXPOSE 5900

CMD ["/entrypoint.sh"]
