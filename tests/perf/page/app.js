// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0
"use strict";

// ?load=N scales the work: 0 is an idle page, 1 a busy app, 5 saturates a 4-core machine.
const LOAD = Math.max(0, Number(new URLSearchParams(location.search).get("load") ?? 1));
const report = { frames: 0, lags: [], keys: [], bundle: typeof bundleChecksum === "function" ? bundleChecksum() : -1 };

// Constant re-rendering, React-style: update 300 x LOAD cards every 100 ms.
const feed = document.getElementById("feed");
const CARDS = 300 * LOAD;
for (let i = 0; i < CARDS; i++) {
  const card = document.createElement("div");
  card.className = "card";
  card.innerHTML = `<b>user${i}</b> <span>0</span> likes <i>caption ${i}</i>`;
  feed.appendChild(card);
}
let tick = 0;
setInterval(() => {
  tick++;
  for (let i = tick % 3; i < CARDS; i += 3) {
    const card = feed.children[i];
    card.style.setProperty("--h", (i * 7 + tick * 13) % 360);
    card.children[1].textContent = String((i * tick) % 9973);
  }
}, 100);

// Many small timers (analytics, polling, animations).
for (let i = 0; i < 4 * LOAD; i++) {
  setInterval(() => { let x = 0; for (let j = 0; j < 2000; j++) x += Math.sqrt(j * i); window.__sink = x; }, 16 + i);
}

// Failing realtime connection and API retries, like a logged-out session.
function reconnect() {
  try {
    const socket = new WebSocket("ws://127.0.0.1:9/realtime");
    socket.onerror = socket.onclose = () => setTimeout(reconnect, 500);
  } catch (e) { setTimeout(reconnect, 500); }
}
if (LOAD > 0) {
  reconnect();
  setInterval(() => fetch("/api/missing?" + Math.random()).catch(() => {}), 300);
}

// Frame rate and event-loop lag.
(function frame() { report.frames++; requestAnimationFrame(frame); })();
(function lag() {
  const start = performance.now();
  setTimeout(() => { report.lags.push(performance.now() - start - 50); lag(); }, 50);
})();

// Key arrival times (wall clock, comparable with the driver's).
document.addEventListener("keydown", (event) => {
  report.keys.push({ key: event.key, t: Date.now() });
}, true);

// Ship the numbers to the test server once a second.
setInterval(() => {
  const body = JSON.stringify({ ...report, t: Date.now() });
  report.frames = 0; report.lags = []; report.keys = [];
  fetch("/report", { method: "POST", body }).catch(() => {});
}, 1000);
