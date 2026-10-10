// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0
//
// Our own browser-automation checks (in the spirit of the public
// intoli / bot.sannysoft / fpcollect lists) plus a behavioral analyzer for
// pointer, scroll and keyboard dynamics. Everything here is a pure function of
// the current page environment or of a recorded event list, so the same code
// backs both the real run and the negative controls.

// A single check returns {name, flagged, detail}. `flagged` true means "this
// looks like a bot/automation".
function check(name, flagged, detail) {
  return { name, flagged: !!flagged, detail: detail === undefined ? null : detail };
}

// What the user agent claims, so checks are scored against the browser we say
// we are (Firefox-family) instead of assuming Chrome.
function uaProfile() {
  const ua = navigator.userAgent || "";
  return {
    ua,
    isFirefox: /Firefox/.test(ua) || /Servo/.test(ua),
    isChrome: /Chrome\//.test(ua) && !/Edg|OPR/.test(ua),
    claimsLinux: /Linux|X11/.test(ua),
    claimsWindows: /Windows/.test(ua),
    claimsMac: /Macintosh|Mac OS X/.test(ua),
  };
}

// The classic automation checks. Each is written so that a normal browser of
// the kind our UA claims is NOT flagged; only genuine automation artifacts are.
export function ownChecks() {
  const p = uaProfile();
  const out = [];
  const add = (name, flagged, detail) => out.push(check(name, flagged, detail));

  // navigator.webdriver: the single most common automation flag.
  add("navigator.webdriver", navigator.webdriver === true, { value: navigator.webdriver });

  // The property should also not be a non-standard truthy value.
  add("webdriver-property-shape", (() => {
    const d = Object.getOwnPropertyDescriptor(Navigator.prototype, "webdriver");
    // Present as a getter (or absent) is fine; a writable data property that
    // some stealth patches leave behind is suspicious.
    return !!(d && "value" in d && d.writable === true && d.value);
  })());

  // navigator.languages must be a non-empty array consistent with language.
  add("navigator.languages-empty", !Array.isArray(navigator.languages) || navigator.languages.length === 0,
    { languages: navigator.languages, language: navigator.language });

  // Headless Chrome historically had languages/plugins stripped. For a
  // Firefox-family UA, empty plugins is normal, so only flag the UA/plugins
  // *contradiction*: a Chrome UA with zero plugins and zero mimeTypes.
  const pluginCount = (navigator.plugins && navigator.plugins.length) || 0;
  const mimeCount = (navigator.mimeTypes && navigator.mimeTypes.length) || 0;
  add("chrome-ua-without-plugins", p.isChrome && pluginCount === 0 && mimeCount === 0,
    { isChrome: p.isChrome, pluginCount, mimeCount });

  // window.chrome must exist iff we claim to be Chrome.
  add("chrome-object-ua-mismatch", p.isChrome && typeof window.chrome === "undefined",
    { isChrome: p.isChrome, hasChrome: typeof window.chrome !== "undefined" });

  // UA platform vs navigator.platform.
  const platform = navigator.platform || "";
  const platformOk =
    (p.claimsLinux && /Linux|arm|aarch|x86|^$/i.test(platform)) ||
    (p.claimsWindows && /Win/i.test(platform)) ||
    (p.claimsMac && /Mac/i.test(platform)) ||
    platform === "";
  add("ua-platform-mismatch", !platformOk, { ua_os: p.claimsLinux ? "linux" : p.claimsWindows ? "windows" : p.claimsMac ? "mac" : "?", platform });

  // navigator.productSub is "20100101" on Gecko and "20030107" on WebKit/Blink.
  // A Firefox-family UA with the Blink value is a tell.
  add("productSub-mismatch", p.isFirefox && navigator.productSub && navigator.productSub !== "20100101",
    { productSub: navigator.productSub });

  // Function.prototype.bind is present in every real browser (missing in old
  // PhantomJS).
  add("missing-bind", typeof Function.prototype.bind !== "function");

  // eval.toString().length is a stable per-engine constant (33 on
  // Gecko/WebKit, 37 on Blink/V8). Zero or absurd values flag emulated evals.
  const evalLen = (() => { try { return eval.toString().length; } catch (e) { return -1; } })();
  add("eval-length-implausible", evalLen !== 33 && evalLen !== 37 && evalLen !== 39, { evalLength: evalLen });

  // An error stack should exist and name the source.
  const stackOk = (() => {
    try { return typeof new Error("x").stack === "string" && new Error("x").stack.length > 0; }
    catch (e) { return false; }
  })();
  add("missing-error-stack", !stackOk);

  // outerWidth/outerHeight of 0 is a headless hallmark (no window chrome).
  add("zero-outer-window", (window.outerWidth === 0 || window.outerHeight === 0),
    { outerWidth: window.outerWidth, outerHeight: window.outerHeight });

  // screen dimensions must be sane and not smaller than the viewport.
  const screenSane = screen && screen.width >= window.innerWidth && screen.height >= window.innerHeight &&
    screen.width > 0 && screen.height > 0;
  add("implausible-screen", !screenSane,
    { screen: screen && [screen.width, screen.height], inner: [window.innerWidth, window.innerHeight] });

  // hairline check: a UA that literally contains automation keywords.
  add("ua-automation-keyword", /headlesschrome|phantomjs|electron|slimerjs|selenium|webdriver/i.test(p.ua),
    { ua: p.ua });

  // notification permission "denied" while Notification is present is the
  // headless-Chrome giveaway; only meaningful for a Chrome UA.
  const notifQuirk = p.isChrome && typeof Notification !== "undefined" &&
    Notification.permission === "denied" && navigator.permissions;
  add("chrome-notification-quirk", !!notifQuirk);

  return out;
}

// --- Behavioral analysis ------------------------------------------------------

// A recorded event: {type, x, y, t}. The analyzer decides whether a stream of
// pointer moves looks human (curved, variable cadence, non-constant speed) or
// robotic (teleport/linear, constant cadence). Pure function: the negative
// control feeds it synthetic robotic data.
export function analyzeMotion(moves) {
  const detail = { count: moves.length };
  if (moves.length < 8) {
    return check("behavior:insufficient-motion", true, { ...detail, reason: "too few pointer samples" });
  }
  // Inter-event timing: humans have jittery sampling; robots are periodic or
  // instantaneous.
  const dts = [];
  for (let i = 1; i < moves.length; i++) dts.push(moves[i].t - moves[i - 1].t);
  const meanDt = dts.reduce((a, b) => a + b, 0) / dts.length;
  const varDt = dts.reduce((a, b) => a + (b - meanDt) ** 2, 0) / dts.length;
  const cvDt = meanDt > 0 ? Math.sqrt(varDt) / meanDt : 0; // coefficient of variation
  detail.meanDt = +meanDt.toFixed(2);
  detail.cvDt = +cvDt.toFixed(3);

  // Speed should vary (accelerate then decelerate), not be constant.
  const speeds = [];
  for (let i = 1; i < moves.length; i++) {
    const dx = moves[i].x - moves[i - 1].x, dy = moves[i].y - moves[i - 1].y;
    const dt = Math.max(moves[i].t - moves[i - 1].t, 0.0001);
    speeds.push(Math.hypot(dx, dy) / dt);
  }
  const meanSpeed = speeds.reduce((a, b) => a + b, 0) / speeds.length;
  const varSpeed = speeds.reduce((a, b) => a + (b - meanSpeed) ** 2, 0) / speeds.length;
  const cvSpeed = meanSpeed > 0 ? Math.sqrt(varSpeed) / meanSpeed : 0;
  detail.cvSpeed = +cvSpeed.toFixed(3);

  // Straightness: ratio of path length to straight-line distance. A perfectly
  // straight (robotic) move has ratio ~1.0; human arcs are longer.
  let pathLen = 0;
  for (let i = 1; i < moves.length; i++) pathLen += Math.hypot(moves[i].x - moves[i - 1].x, moves[i].y - moves[i - 1].y);
  const straight = Math.hypot(moves[moves.length - 1].x - moves[0].x, moves[moves.length - 1].y - moves[0].y);
  const detour = straight > 1 ? pathLen / straight : 1;
  detail.detourRatio = +detour.toFixed(3);

  // All trusted?
  const untrusted = moves.filter((m) => m.trusted === false).length;
  detail.untrusted = untrusted;

  // Robotic if timing is too regular AND speed is too constant AND the path is
  // dead straight; or if any event was untrusted.
  const tooRegular = cvDt < 0.08;
  const tooConstant = cvSpeed < 0.12;
  const tooStraight = detour < 1.02;
  const robotic = untrusted > 0 || (tooRegular && tooConstant) || (tooStraight && tooConstant);
  detail.flags = { tooRegular, tooConstant, tooStraight, untrusted: untrusted > 0 };
  return check("behavior:motion", robotic, detail);
}

// Synthetic robotic motion for the negative control: a straight line sampled at
// a constant rate with constant speed.
export function syntheticRoboticMotion() {
  const moves = [];
  for (let i = 0; i <= 40; i++) moves.push({ type: "mousemove", x: 100 + i * 10, y: 100 + i * 10, t: i * 16, trusted: true });
  return moves;
}
