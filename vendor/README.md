# Patched Servo crates

servoaf builds on the published Servo 0.7.0 crates. Where 0.7.0 has a bug that
upstream Servo has already fixed, the affected crate is vendored here with the
upstream fix applied. `[patch.crates-io]` in the root `Cargo.toml` points
Cargo at it. Drop the crate (and its `[patch]` line) once a Servo release on
crates.io contains the fix.

## `servo-script` 0.7.0

The crate as published on crates.io, plus:

| Upstream change | Why |
|---|---|
| [servo/servo#48558](https://github.com/servo/servo/pull/48558) "Preallocate timer array" | Prerequisite of the next one (same function). |
| [servo/servo#48548](https://github.com/servo/servo/pull/48548) "Ensure pending timer callbacks are always reachable from GC roots" (fixes servo/servo#48538 and servo/servo#20452) | When several timers were due at once, Servo moved them, JS callbacks included, into a list the garbage collector couldn't see. A GC during the first callback could then free or move the next callback's function, and calling it segfaulted the script thread (`Caught signal 11 in thread "Script#1"`, in `JS::GetNonCCWObjectGlobal`). That took down the whole browser. Heavy sites with many timers (Instagram, for example) hit it within seconds. |

Only `event_loop/timers.rs` and `dom/globalscope/globalscope.rs` differ from the
published crate. Both are taken verbatim from upstream commit `3ae004938`; no
other commit touches them between 0.7.0 and the fix. The combined diff is
in `patches/servo-script-timer-gc-roots.patch`. The crate's `Cargo.lock`
and `.cargo_vcs_info.json` were dropped (unused when vendored).

Check the vendored crate against crates.io:

```sh
mkdir -p /tmp/servo-script && curl -sL https://crates.io/api/v1/crates/servo-script/0.7.0/download \
  | tar -xz -C /tmp/servo-script
diff -r /tmp/servo-script/servo-script-0.7.0 vendor/servo-script
```

The regression test `tests/regressions/timer_gc_crash.py` crashes unpatched
servoshell 0.7.0 within a second and must pass with servoaf.
