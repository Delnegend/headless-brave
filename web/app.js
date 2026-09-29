// A bare window onto the session: no controls, no settings, no way out.
// Opening the page is the whole interaction — it attaches to the VNC server
// and shows the screen the browser is using.
//
// The screen is shared, so watching it never interferes with whatever is
// driving the browser over CDP, and never takes the screen away from anyone
// else watching it.

import RFB from "/novnc/core/rfb.js";

const screen = document.getElementById("screen");
const notice = document.getElementById("notice");

// The screen outlives any one viewer, so a dropped connection is worth
// another attempt — but the waits grow, because a viewer that reconnects
// faster than the desktop settles makes for a flicker.
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
  // Whether we asked for it or the server went away, the answer is the same:
  // the screen is shared and will be there again.
  client.addEventListener("disconnect", () => reconnect());
  client.addEventListener("credentialsrequired", () => {
    // We always supply the password up front; being asked again means it was
    // not accepted, so say so rather than asking a viewer who cannot answer.
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
