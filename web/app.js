// noVNC web viewer attaching to /websockify.

import RFB from "/novnc/core/rfb.js";

const screen = document.getElementById("screen");
const notice = document.getElementById("notice");

// Exponential backoff reconnect intervals in milliseconds.
const RETRY_DELAYS = [1000, 2000, 4000, 8000, 16000, 30000];

let rfb = null;
let retry = null;
let attempt = 0;
let scaleTimer = null;

function announce(message) {
  notice.textContent = message ?? "";
  notice.style.display = message ? "grid" : "none";
}

/** Adds a hint to what is already on screen rather than replacing it, so the
 *  reason a session ended stays readable while it is being retried. */
function hint(text) {
  const said = notice.textContent.replace(/\s*\(retrying…\)$/, "");
  announce(said ? `${said} (${text})` : text);
}

function bridgeUrl() {
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${scheme}//${window.location.host}/websockify`;
}

/** Fits the screen to the window. noVNC keeps the canvas at the remote
 *  resolution, so this is only ever a scale-down. */
function fit() {
  if (!rfb) return;
  rfb.scaleViewport = true;
  const frame = screen.firstElementChild;
  if (!frame) return;
  const available = screen.getBoundingClientRect();
  const width = frame.offsetWidth;
  const height = frame.offsetHeight;
  if (width === 0 || height === 0) return;
  const scale = Math.min(1, available.width / width, available.height / height);
  frame.style.transform = `scale(${scale})`;
}

function connect(password) {
  const client = new RFB(screen, bridgeUrl(), { credentials: { password } });
  rfb = client;

  client.addEventListener("connect", () => {
    attempt = 0;
    announce("");
    fit();
  });
  client.addEventListener("disconnect", () => reconnect());
  client.addEventListener("credentialsrequired", () => {
    announce("the VNC password was rejected");
  });
  client.scaleViewport = true;
}

function reconnect() {
  if (retry !== null) return;
  if (attempt >= RETRY_DELAYS.length) {
    announce("the desktop is not answering — reload to try again.");
    return;
  }

  hint("retrying…");
  retry = window.setTimeout(() => {
    retry = null;
    attempt += 1;
    start();
  }, RETRY_DELAYS[attempt]);
}

async function start() {
  try {
    const { vnc } = await (await fetch("/api/config", { cache: "no-store" })).json();
    connect(vnc.password);
  } catch (error) {
    announce(`cannot read the connection settings: ${error}`);
    reconnect();
  }
}

window.addEventListener("resize", () => {
  if (scaleTimer !== null) window.clearTimeout(scaleTimer);
  scaleTimer = window.setTimeout(() => {
    scaleTimer = null;
    fit();
  }, 120);
});

window.addEventListener("beforeunload", () => rfb?.disconnect());

start();
