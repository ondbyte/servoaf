# servoaf

**servoaf** is Servo's own browser, [servoshell](https://github.com/servo/servo/tree/main/ports/servoshell),
with a **Model Context Protocol (MCP) server built in**. You get the full
browser (URL bar, tabs, back/forward/reload, dialogs, file pickers, the same
engine features and performance as servoshell), and an AI agent can drive the
same window over MCP at the same time. It is the Servo sibling of
[chromeaf](https://github.com/ondbyte/chromeaf): the same MCP tools, with the
same names, arguments and results, on a different engine. An AI agent (or a
script, or a person with `curl`) can drive the browser over HTTP. It can read
pages, find clickable elements, take screenshots, move the mouse like a human,
click, type, scroll, manage tabs and answer page dialogs.

To websites it is a normal browser. The control layer:

- does **not** inject or evaluate any script into a page to drive it, and
  opens no WebDriver session and no DevTools protocol;
- sends mouse, wheel and keyboard input through `WebView::notify_input_event`,
  the same entry point real OS input takes. Events are hit-tested like real
  ones, and pages see ordinary `isTrusted` events;
- reads the DOM through Servo's built-in, natively implemented script-thread
  commands (`webdriver_handlers.rs`: query selectors, element rects, text,
  attributes, computed style, page source), called directly from the embedder.

servoaf is not identified as a bot by the common non-CAPTCHA detectors
(Fingerprint BotD, fpscanner, isbot) or by the classic automation checks — see
[the bot-detection test](tests/bot-detection/). To get there it ships one small
built-in user script that sets `navigator.webdriver = false` (the value real
browsers report; Servo leaves it unset) and reports a real screen and window
size. The remaining gap is the TLS/JA4 network fingerprint, which comes from
Servo's rustls stack; that one is documented, not yet closed.

It is a plain Cargo workspace on top of the published
[`servo` crates](https://crates.io/crates/servo) (0.7.0): servoshell is vendored
from Servo v0.7.0 with small, marked patches (see
[`crates/servoaf-shell/VENDORED.md`](crates/servoaf-shell/VENDORED.md)), built with
the same features as servoshell (WebGL, WebCrypto, WebXR, gamepad, jemalloc, …)
and Servo's own `Cargo.lock`. There is no Servo checkout and no engine patches.

## Download a prebuilt build (Linux x86-64)

A stripped, self-contained build is checked in under [`dist/`](dist/):
`dist/servoaf-0.1.0-x86_64-linux.tar.xz` (~37 MB), with a `.sha256` next to it.
It needs **glibc 2.39 or newer**: Ubuntu 24.04+, Debian 13+, Fedora 40+ or a
recent Arch.

```sh
base=https://github.com/ondbyte/servoaf/raw/claude/ecstatic-planck-51szvk/dist
curl -LO $base/servoaf-0.1.0-x86_64-linux.tar.xz
curl -LO $base/servoaf-0.1.0-x86_64-linux.tar.xz.sha256
sha256sum -c servoaf-0.1.0-x86_64-linux.tar.xz.sha256
tar xf servoaf-0.1.0-x86_64-linux.tar.xz
cd servoaf-0.1.0-x86_64-linux
./install.sh                      # -> ~/.local/bin/servoaf (PREFIX=... to change)
servoaf https://example.com       # window + MCP on http://127.0.0.1:9223/mcp
```

You can also run `./servoaf` straight from the extracted folder.
`install.sh` warns about missing runtime libraries. Desktops almost always have
them; on a minimal system install:

- Debian/Ubuntu: `sudo apt install libegl1 libgl1-mesa-dri libfontconfig1 libfreetype6 libxkbcommon-x11-0 libudev1 libcap2`
- Fedora: `sudo dnf install mesa-libEGL mesa-dri-drivers fontconfig freetype libxkbcommon-x11 systemd-libs libcap`

If an older build printed `xkbcommon: ERROR: …/iso8859-1/Compose … not a
valid UTF-8 string`, your locale has no `.UTF-8` suffix (check with `locale`).
That only disabled dead keys in the window. Current builds switch the
keyboard locale to UTF-8 automatically; you can also set
`LANG=en_US.UTF-8` (or your language's `.UTF-8` locale).

For older distributions or other architectures, build from source as shown
below. `make package` regenerates the archive.

## Quick start (Linux)

```sh
git clone https://github.com/ondbyte/servoaf && cd servoaf
make deps          # system packages (sudo) + the pinned Rust toolchain
make build         # cargo build --release  ->  target/release/servoaf
make run           # window + MCP on http://127.0.0.1:9223/mcp
make run-headless  # same without a display
make test          # unit, smoke, regression and bot-detection tests
make bot-detection # fail if industry-standard detectors flag servoaf as a bot
make perf          # input latency / fps / CPU on a heavy page (UPSTREAM=servoshell to compare)
```

`make deps` runs `scripts/install-deps.sh`, which supports apt (Debian/Ubuntu),
dnf (Fedora) and pacman (Arch). Use `make check-deps` to only report what is
missing. The first build compiles Servo and takes a while: about 45 minutes on
4 cores, 8+ GB RAM and ~10 GB of disk for `target/`. Later builds of
servoaf itself take seconds. `make docker-build` builds inside an Ubuntu 24.04
image instead.

The result is a single self-contained binary (Servo's resources are baked in).
It takes all of servoshell's options (`servoaf --help`), plus the MCP ones:

```sh
target/release/servoaf [options] [URL]

  --mcp-port=PORT           Serve MCP on http://HOST:PORT/mcp (default 9223; 0 disables)
  --mcp-host=HOST           Address to bind (default 127.0.0.1)
  --mcp-token=TOKEN         Require 'Authorization: Bearer TOKEN' (or $SERVOAF_MCP_TOKEN)
  --no-cursor-overlay       Don't draw the agent's pointer over the page
  --no-webdriver-shim       Don't define navigator.webdriver=false (leave it unset like Servo)
  --auto-dismiss-dialogs    Accept alerts, cancel confirm/prompt automatically
  --tool-timeout=SECONDS    Give up on a tool call after this long (default 120)
  --quiet-console           Don't print the pages' console messages
  --user-data-dir=DIR       Profile directory (alias of servoshell's --config-dir)

  servoshell's own, e.g.:
  -z, --headless            No window; render offscreen (no display needed)
  --window-size=WxH         Initial window size (default 1024x740)
  --config-dir=DIR          Profile directory (default ~/.config/servo/default)
  --pref=NAME[=VALUE]       Set a Servo preference, e.g. --pref=dom_webgpu_enabled
```

Make variables map onto these: `make run MCP_PORT=9333 MCP_TOKEN=secret
URL=example.com SIZE=1440x900 USER_DATA_DIR=~/.servoaf`. `make package`
builds with `PROFILE=production` (whole-program LTO, like Servo's official
builds; slow to build) and produces `dist/servoaf-<version>-<arch>-linux.tar.xz`
and its `.sha256`.

Like servoshell, servoaf keeps its profile (cookies, storage) in
`~/.config/servo/default` unless `--user-data-dir`/`--config-dir` says
otherwise. Everything the agent does happens in the visible window and tabs:
tabs it opens appear in the tab strip, tabs you open show up in `get-tabs`, and
dialogs it answers close in the UI. The server binds to loopback by default. It rejects requests
whose `Host`/`Origin` is not loopback (DNS-rebinding protection) and can
require a bearer token. **Anyone who can reach the port controls the browser**,
so only bind elsewhere with a token, behind a firewall. Set `RUST_LOG=warn` (or
`info`, `servoaf_core=debug`) for logs.

### Connecting an MCP client

The endpoint speaks MCP *Streamable HTTP* (JSON responses) at `/mcp`, protocol
versions 2025-06-18, 2025-03-26 and 2024-11-05:

```sh
claude mcp add --transport http servoaf http://127.0.0.1:9223/mcp
```

```sh
curl -s localhost:9223/mcp -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
       "params":{"name":"get-tabs","arguments":{}}}'
```

## Tools

Coordinates are CSS pixels relative to the top-left of the tab's viewport (the
same numbers `getBoundingClientRect()` gives). Every page and input tool takes
an optional `tab_id`; the default is the active tab.

| Tool | What it does |
|---|---|
| `get-tabs` | The window and its tabs: `tab_id`, title, url, active, loading |
| `create-tab` | Open a tab (`url`, `foreground`, `wait_for_load`, `timeout_ms`) |
| `close-tab` / `activate-tab` | Close / switch to a tab |
| `navigate` | Load a URL as if typed into the address bar (`example.com` works; waits for load) |
| `go-back` / `go-forward` / `reload` / `wait-for-load` | History and load control |
| `get-whole-page-as-html` | Live DOM serialized as HTML |
| `get-page-text` | Visible text of the page |
| `get-clickables` | Links, buttons, inputs, ARIA widgets, click handlers and pointer-cursor elements with `id`, label, bounds and center |
| `get-screen-shot` | PNG/JPEG of the viewport, optional clip and downscale |
| `move-cursor` | Human-like pointer movement to `x,y` or `clickable_id` |
| `click` / `rightclick` / `double-click` | Move there, then press, hold and release like a person |
| `scroll` | Mouse-wheel scrolling by `delta_x`/`delta_y` pixels |
| `type-text` | Types into the focused element key by key with human timing |
| `press-key` | Keys and chords: `Enter`, `Escape`, `ArrowDown`, `Control+a`, … |
| `get-cursor-position` | Where the synthetic pointer currently is |
| `get-dialogs` | Open alert/confirm/prompt dialogs, `<select>` popups, context menus, file and color pickers |
| `respond-dialog` | Accept or dismiss one: prompt `text`, `<select>` `option_ids`, menu `item`, file `paths` |

A typical loop is `get-clickables` → `click {clickable_id}` → `type-text` →
`press-key Enter` → `get-page-text` / `get-screen-shot`.

The last two tools are servoaf-specific. Servo hands dialogs and popups to the
embedder instead of drawing them. A page that calls `alert()` stays paused
until the dialog is answered; page tools then fail immediately with the
`dialog_id` to answer. Clicking a `<select>` opens a popup that you answer with
`respond-dialog {option_ids: [...]}`. `--auto-dismiss-dialogs` handles
alert/confirm/prompt automatically.

Screenshots and input go to the visible tab, so tools that need them bring a
background tab to the front first. Reading tools work on any tab.

### How the human-like input works

The input model is ported from chromeaf. `move-cursor` (and the movement part
of every click) follows a cubic Bézier path with:

- randomized curvature;
- a minimum-jerk velocity profile (slow start, fast middle, slow arrival);
- small hand tremor that fades near the target;
- irregular 7–16 ms sampling;
- a duration from Fitts's law, based on distance and target size;
- an occasional small overshoot-and-correct on long moves.

Clicks aim near, not exactly at, the element center. They hover briefly and
hold the button for 55–130 ms. Typing uses per-key delays with longer pauses at
word breaks, and presses Shift for capitals and symbols. Operations on one tab
are queued so that concurrent tool calls never interleave their events. In a
window, a small pointer drawn on top of the page (not part of it, and not in
screenshots) shows the agent's cursor. Disable it with `--no-cursor-overlay`.

## Repository layout

```
Makefile                    entry point (make help)
scripts/install-deps.sh     system packages + Rust toolchain
scripts/mcp_smoke_test.py   end-to-end test of every tool (stdlib only)
Dockerfile                  Ubuntu 24.04 build image
crates/servoaf-core/        platform-independent MCP library (no windowing)
  src/mcp/                  Streamable HTTP transport, JSON-RPC/MCP protocol, tool registry
  src/host.rs               what the browser shell provides: tabs, dialogs (Host trait)
  src/browser.rs            per-tab automation state, events from the shell, MCP pump
  src/dom.rs                DOM reads through Servo's native script-thread commands
  src/input/                human mouse-path model, key mapping, input injection
  src/tools/                tab, page, screenshot, input and dialog tools
  src/runtime.rs            single-threaded executor driven by the shell's event loop
crates/servoaf-shell/       the servoaf binary: servoshell v0.7.0 + MCP (VENDORED.md)
  mcp.rs                    Host implementation over servoshell's windows, tabs, dialogs
tests/regressions/          crash regressions (abandoned DOM requests, timers vs GC)
tests/perf/                 responsiveness benchmark against upstream servoshell
vendor/                     Servo crates with upstream fixes backported (see vendor/README.md)
tests/bot-detection/        bot-detection test suite
```

## Architecture

```
 MCP client ──HTTP──► mcp::http (tiny_http, own threads)
                          │ JSON-RPC body over a channel, wakes the main loop
                          ▼  (main thread)
                      McpProtocol ──► ToolRegistry ──► tools/*  (futures on runtime.rs)
                                                        │
             ┌──────────────────────┬───────────────────┼─────────────────────┐
             ▼                      ▼                   ▼                     ▼
     Host (servoshell's      dom.rs  ──Servo──►  script thread       input/*
     windows, tabs,          WebDriverScriptCommand (native Rust,   WebView::
     dialogs: mcp.rs)        no page JS)                             notify_input_event
```

Servo's `WebView`s live on the main thread, so tool calls run as futures on a
small executor that servoshell's event loop polls each turn (`Browser::pump`,
woken by the HTTP server and a timer thread). servoshell owns the windows,
tabs and dialogs; servoaf-core reaches them through the `Host` trait and keeps
only automation state (cursor, clickable ids, load tracking) per tab. Windowed
mode is servoshell's winit + egui UI; `--headless` is its offscreen software
renderer (Mesa llvmpipe).

## Android (planned)

The vendored servoshell already contains Servo's Android/OpenHarmony port
(`crates/servoaf-shell/egl/`), and `servoaf-core` has no windowing dependency.
The Android build will hook the same `Mcp` attachment into that port's event
loop, package it with Servo's Android app shell (`cargo ndk` + Gradle), and
reach the MCP server with `adb forward tcp:9223 tcp:9223`.

## Performance and stability

servoaf is servoshell (Servo's own browser) plus the MCP server, built from the
same Servo 0.7.0 crates with the same features. `make perf UPSTREAM=...` runs
[`tests/perf/responsiveness.py`](tests/perf/responsiveness.py) to compare it
with an unmodified servoshell. The test opens a heavy single-page-app stand-in,
types into it with `xdotool` like a person, and measures key-to-`keydown`
latency, frame rate, event-loop lag and CPU. Measured on a 4-core VM with
software GL (Xvfb), median of 4 interleaved runs:

| Page | Browser | fps | key latency p50 / p95 | CPU | Crashed |
|---|---|---|---|---|---|
| idle (`--load 0`) | servoshell 0.7.0 | 33 | 20 / 33 ms | 2 % | 0 / 1 |
| idle (`--load 0`) | servoaf | 33 | 19 / 32 ms | 2 % | 0 / 1 |
| busy app (`--load 1`) | servoshell 0.7.0 | 2 | 2.3 / 6.6 s | 331 % | **3 / 4** |
| busy app (`--load 1`) | servoaf | 12 | 1.7 / 3.4 s | 226 % | 0 / 4 |
| busy app (`--load 1`) | servoaf, MCP client connected | 12 | 0.9 / 3.2 s | 236 % | 0 / 4 |

- **servoaf adds no input or rendering overhead.** On an idle page the two are
  identical. With the MCP server idle, servoaf's code does no work and never
  wakes the event loop.
- **Servo 0.7.0 itself crashes on timer-heavy pages.** A GC-rooting bug in its
  timer code segfaults the page's script thread (`Caught signal 11 in thread
  "Script#1"`), which takes down the whole browser. servoaf ships the upstream
  fix ([vendor/](vendor/README.md), servo/servo#48548), and
  [`tests/regressions/timer_gc_crash.py`](tests/regressions/timer_gc_crash.py)
  crashes unpatched servoshell within a second.
- **Heavy sites are still slow in Servo.** Under load, input waits behind
  Servo's layout and painting, so a key press can take seconds to reach the
  page. That is the same in servoshell; a real GPU helps a lot compared with
  the software GL measured here. Sites such as Instagram are at the edge of
  what Servo handles today.
- Prebuilt archives use the `production` profile: whole-program LTO and one
  codegen unit, like Servo's official builds.

If servoaf exits, it says why on stderr: `servoaf: closing a window: ...`, `tab
closed by the page or engine: URL`, `Caught signal ...` with a backtrace for a
crash, and `servoaf: exited` at the end. Page console messages print once, or
not at all with `--quiet-console`. Messages such as Instagram's `ErrorUtils
caught an error ... UNAUTHENTICATED` come from the site's own JavaScript (its
live-update connection is refused while logged out). They are not browser
errors.

## Limitations

- Servo's web compatibility is good and improving, but it is not Chromium's;
  some sites will not work.
- Servo resolves each element reference by walking the document, so DOM work is
  bounded. `get-clickables` looks for pointer-cursor widgets among the first
  1,500 elements. In viewport mode it stops once results are below the fold,
  but always checks the end of the page for fixed banners. With
  `include_offscreen` on very large pages (thousands of interactive elements)
  it can take tens of seconds.
- The scroll offset is read from the root element's `scrollTop`/`scrollLeft`
  properties. A page that overrides those getters on `Element.prototype` could
  notice the read.
- Media playback (GStreamer) is not enabled, as in default servoshell builds.
  Headless rendering uses software GL.
- `get-tabs` lists the tabs of all windows under one `window_id`.
- The TLS/JA4 network fingerprint comes from Servo's rustls stack and does not
  match the Firefox the user agent claims. It can't be changed from this
  workspace (it lives in Servo's `net` crate); the
  [bot-detection test](tests/bot-detection/) reports it as a documented gap.

## License

MPL-2.0, like Servo. See [LICENSE](LICENSE).
