# Vendored servoshell

This crate is [servoshell](https://github.com/servo/servo/tree/v0.7.0/ports/servoshell),
Servo's own browser shell, from **Servo v0.7.0** (commit
`aac43a3f31a259f04a574f5ec4e959c943ad7cc7`, MPL-2.0), built as the `servoaf`
binary on top of the published Servo 0.7.0 crates. servoaf is servoshell plus
the MCP control server, so it has the same UI, features and performance.

## What servoaf changes

Upstream files are kept byte-for-byte except for small patches, each marked
`servoaf`. `scripts/servoshell-diff.sh` prints the complete diff against
upstream.

| File | Change |
|---|---|
| `Cargo.toml` | Workspace dependencies resolved to Servo 0.7.0's versions; package/binary named `servoaf`; `servoaf-core` added; Servo's `testbinding` feature dropped (test-only DOM globals). |
| `mcp.rs` (new) | Attaches `servoaf_core` to servoshell: a `Host` over its windows/tabs/dialogs, the HTTP server, and a timer that wakes the event loop for tool timers. |
| `lib.rs` | `mod mcp`. |
| `prefs.rs` | `--mcp-port`, `--mcp-host`, `--mcp-token`, `--no-cursor-overlay`, `--no-webdriver-shim`, `--auto-dismiss-dialogs`, `--tool-timeout`, `--quiet-console`, `--user-data-dir` (alias of `--config-dir`). |
| `desktop/cli.rs` | Use a UTF-8 locale for the keyboard Compose table (silences libxkbcommon errors with e.g. `LANG=en_US`); print `servoaf: exited` when the event loop ends. |
| `desktop/app.rs` | Start the MCP server; add the `navigator.webdriver = false` user script. |
| `running_app_state.rs` | Own the MCP server, pump it each event-loop turn, forward load/url/crash events, `--auto-dismiss-dialogs`; a headless instance driven over MCP keeps running with no tabs; say on stderr why a window closes and when the page/engine closes a tab. |
| `window.rs` | `PlatformWindow::visit_mcp_dialogs`; `close_requested()`. |
| `desktop/headed_window.rs`, `desktop/dialog.rs` | MCP can read and answer the dialogs shown in the GUI. Page console messages print once (upstream also logs them, printing errors twice), or not at all with `--quiet-console`. |
| `desktop/headless_window.rs` | Keep dialogs for MCP instead of dropping them. |
| `desktop/gui.rs` | Draw the agent's pointer over the page. |
| `build.rs`, `resources/icons/` | Window/app icons vendored from Servo's repo root. |

## Formatting

Servo formats with nightly-only rustfmt options, so **don't run rustfmt on the
upstream files** (it would rewrap lines and bury the patches in noise).
`make fmt` formats only servoaf's own code.

## Updating to a new Servo release

1. Bump the `servo*` versions (and other dependencies) in `Cargo.toml` and
   reseed `Cargo.lock` from that Servo release's `Cargo.lock`.
2. `scripts/servoshell-diff.sh > servoaf.patch` (against the old version), copy
   the new `ports/servoshell` over this directory, and re-apply the patch.
3. Build, then run `make test`.
