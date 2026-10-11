# servoaf

**servoaf** is a [Servo](https://servo.org)-based browser with a **Model Context
Protocol (MCP) server built in**. It is the Servo sibling of
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
[`servo` crate](https://crates.io/crates/servo) (0.7.0). There is no Servo
checkout and no patches.

## Download a prebuilt build (Linux x86-64)

A stripped, self-contained build is checked in under [`dist/`](dist/):
`dist/servoaf-0.1.0-x86_64-linux.tar.xz` (~32 MB), with a `.sha256` next to it.
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

- Debian/Ubuntu: `sudo apt install libegl1 libgl1-mesa-dri libfontconfig1 libfreetype6 libxkbcommon-x11-0`
- Fedora: `sudo dnf install mesa-libEGL mesa-dri-drivers fontconfig freetype libxkbcommon-x11`

For older distributions or other architectures, build from source as shown
below. `make package` regenerates the archive.

## Quick start (Linux)

```sh
git clone https://github.com/ondbyte/servoaf && cd servoaf
make deps          # system packages (sudo) + the pinned Rust toolchain
make build         # cargo build --release  ->  target/release/servoaf
make run           # window + MCP on http://127.0.0.1:9223/mcp
make run-headless  # same without a display
make test          # unit tests + smoke test + bot-detection test
make bot-detection # fail if industry-standard detectors flag servoaf as a bot
```

`make deps` runs `scripts/install-deps.sh`, which supports apt (Debian/Ubuntu),
dnf (Fedora) and pacman (Arch). Use `make check-deps` to only report what is
missing. The first build compiles Servo and takes a while: about 30 minutes on
4 cores, 8+ GB RAM and ~6 GB of disk for `target/`. Later builds of
servoaf itself take seconds. `make docker-build` builds inside an Ubuntu 24.04
image instead.

The result is a single self-contained binary (Servo's resources are baked in):

```sh
target/release/servoaf [options] [URL]

  --mcp-port PORT           Serve MCP on http://HOST:PORT/mcp (default 9223; 0 disables)
  --mcp-host HOST           Address to bind (default 127.0.0.1)
  --mcp-token TOKEN         Require 'Authorization: Bearer TOKEN' (or $SERVOAF_MCP_TOKEN)
  --headless                No window; render offscreen (no display needed)
  --size WxH                Window/viewport size in CSS px (default 1280x800)
  --user-data-dir DIR       Keep cookies and storage in DIR (default: temporary)
  --no-cursor-overlay       Don't draw the agent's pointer in the window
  --no-webdriver-shim       Don't define navigator.webdriver=false (leave it unset like Servo)
  --auto-dismiss-dialogs    Accept alerts, cancel confirm/prompt automatically
  --tool-timeout SECONDS    Give up on a tool call after this long (default 120)
```

Make variables map onto these: `make run MCP_PORT=9333 MCP_TOKEN=secret
URL=example.com SIZE=1440x900 USER_DATA_DIR=~/.servoaf`. `make package`
produces `dist/servoaf-<version>-<arch>-linux.tar.xz` and its `.sha256`.

Without `--user-data-dir` every run starts with a fresh profile, which is
deleted on exit. The server binds to loopback by default. It rejects requests
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
crates/servoaf-core/        platform-independent library (no windowing)
  src/mcp/                  Streamable HTTP transport, JSON-RPC/MCP protocol, tool registry
  src/browser.rs            Servo, tabs (one WebView each), WebViewDelegate, pending dialogs
  src/dom.rs                DOM reads through Servo's native script-thread commands
  src/input/                human mouse-path model, key mapping, input injection
  src/tools/                tab, page, screenshot, input and dialog tools
  src/runtime.rs            single-threaded executor driven by the embedder loop
crates/servoaf/             Linux binary: CLI, winit window, headless loop
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
        Browser / tabs         dom.rs  ──Servo──►  script thread       input/*
        (WebView per tab,      WebDriverScriptCommand (native Rust,   WebView::
         WebViewDelegate)      no page JS)                             notify_input_event
```

Servo's `WebView`s live on the main thread, so tool calls run as futures on a
small executor that the embedder's event loop polls (`Browser::pump`). The
Linux binary provides that loop twice:

- **Windowed** (`headed.rs`): winit and `WindowRenderingContext`. A person can
  use the window at the same time; mouse and keyboard are forwarded to the page.
- **Headless** (`headless.rs`): `SoftwareRenderingContext`, Mesa llvmpipe.

## Android (planned)

`servoaf-core` has no windowing dependency on purpose. The Android port will
add a `crates/servoaf-android` cdylib, following servoshell's `egl/` port:

- a JNI entry point;
- a `WindowRenderingContext` on the `SurfaceView`;
- an event loop that calls `Browser::pump`;
- the same MCP server, reached with `adb forward tcp:9223 tcp:9223`.

It will be packaged with a small Java/Kotlin activity, built with
`cargo ndk` + Gradle.

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
- Media playback (GStreamer) is not enabled. Headless rendering uses software GL.
- One window; tabs are switched within it.
- The TLS/JA4 network fingerprint comes from Servo's rustls stack and does not
  match the Firefox the user agent claims. It can't be changed from this
  workspace (it lives in Servo's `net` crate); the
  [bot-detection test](tests/bot-detection/) reports it as a documented gap.

## License

MPL-2.0, like Servo. See [LICENSE](LICENSE).
