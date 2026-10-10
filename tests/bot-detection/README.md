# servoaf bot-detection suite

Runs servoaf against industry-standard, **non-CAPTCHA** bot/automation
detectors and fails (non-zero exit) if any of them identify the browser as a
bot or as automated. CAPTCHAs are intentionally excluded — they need a human.

The goal is QA: it tells us exactly which detectors flag servoaf, so a
regression (for example, if a future change re-exposes an automation artifact)
is caught, while known, documented limitations stay visible.

```sh
make bot-detection
# or directly:
python3 tests/bot-detection/run.py --launch target/release/servoaf
python3 tests/bot-detection/run.py --url http://127.0.0.1:9223/mcp   # already running
BOTCHECK_VERBOSE=1 python3 tests/bot-detection/run.py --launch target/release/servoaf  # browser logs
```

`run.py` serves the local detector page, launches servoaf headless (pass
`--headed` for a window), drives it with the MCP tools exactly as an agent
would, reads the verdict the page writes into the DOM, captures the TLS
ClientHello, prints a table and sets the exit code.

## What it checks

Detectors run offline from `vendor/` (see `vendor/README.md` for provenance and
licenses) plus our own checks in `page/checks.js`:

- **Fingerprint BotD** (`botd`) — `detect()` → bot / botKind.
- **fpscanner** (`fpscanner`) — `fastBotDetection` and its per-signal details
  (webdriver, CDP, Playwright, worker/iframe mismatches, SwiftShader renderer,
  headless screen resolution, UTC timezone, …).
- **isbot** (`isbot`) — user-agent classification.
- **own:**… — the classic automation checks (`navigator.webdriver`, plugins /
  mimeTypes vs the claimed UA, `window.chrome` consistency, platform/productSub
  consistency, `Function.prototype.bind`, `eval.toString().length`, error
  stacks, zero outer-window, implausible screen, UA keywords, notification
  quirks). Each is scored against the browser our UA claims (Firefox-family),
  not "must look like Chrome".
- **behavior:motion** — the page records pointer / scroll / key events while
  the driver uses servoaf's human-like `move-cursor` / `click` / `type-text` /
  `scroll`; the analyzer checks the motion is curved, variably timed,
  non-constant-speed and trusted — i.e. not robotic.
- **tls:ja4** — the TLS ClientHello JA4 fingerprint (see below).

### Negative controls

The suite verifies the detectors actually fire, so a green run means something:

- `control:botd-webdriver` — BotD is re-run with the `webDriver` component
  forced true; it **must** report a bot.
- `control:behavior-robotic` — the motion analyzer is fed a perfectly linear,
  constant-cadence synthetic path; it **must** flag it as robotic.

If either control does not fire, the suite fails as `BROKEN`.

## How servoaf passes

Two Servo-level gaps made servoaf look automated; both are fixed in
`crates/servoaf-core/src/browser.rs` (and are plain browser-correctness fixes):

- **`screen` / `window.outerWidth` / `outerHeight` were 0.** servoaf now
  implements `WebViewDelegate::screen_geometry`, reporting a plausible desktop
  screen and window rectangle. Zero values are a classic headless tell that
  BotD, fpscanner and several of our checks key on.
- **`navigator.webdriver` was `undefined`.** The WebDriver spec and real
  browsers expose it as `false`; Servo does not implement it, and its absence
  lets fpscanner's `hasWebdriverWritable` flag the browser. servoaf ships a
  small built-in user script that defines it as a read-only `false`. Disable
  with `--no-webdriver-shim`.

The test launches servoaf with a non-UTC `TZ` (a server defaulting to UTC is
itself a weak signal fpscanner reports; the browser honours `$TZ`).

## Known gap: TLS / HTTP fingerprint (`tls:ja4`)

`tls/ja4_probe.py` captures servoaf's TLS ClientHello and computes its
[JA4](https://github.com/FoxIO-LLC/ja4) fingerprint (for example
`t13i1010h2_61a7ad8aa9b6_0d308c48d2a3`). servoaf drives Servo's network stack,
which uses **rustls**. rustls's ClientHello — cipher and extension ordering, no
GREASE, a lean ~10-cipher list, no `record_size_limit` — is structurally unlike
the **NSS** stack Firefox uses, so the JA4 does not match the Firefox our user
agent claims. The same applies to HTTP/2 header ordering.

This is **not fixable by editing the servoaf workspace**: the ClientHello is
built inside Servo's `net` crate and rustls, both upstream dependencies, and
rustls deliberately does not expose ClientHello ordering to mimic other stacks.
Making it match Firefox would require patching Servo's `net` crate (and
likely rustls, or swapping in a uTLS-style TLS stack). Until then `tls:ja4` is
reported as a **KNOWN GAP**: it prints the captured JA4 and does not fail the
build, so the JavaScript and behavioral checks still gate every run. Run
`python3 tests/bot-detection/tls/ja4_probe.py` standalone to print the current
fingerprint.

## Files

```
run.py              driver: serve page, launch servoaf, drive it, verdict + exit code
page/index.html     loads the vendored detectors + our checks, writes a base64 verdict into the DOM
page/checks.js      our own automation checks and the behavioral analyzer (pure functions)
vendor/             version-pinned BotD, fpscanner, isbot (see vendor/README.md)
tls/ja4_probe.py    TLS ClientHello capture + JA4 computation
```
