#!/usr/bin/env python3
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
"""Regression test: timers that fire together must survive a GC.

Servo 0.7.0 moved all due timers, JS callbacks included, into a Vec the
garbage collector could not see, then ran them one by one. When one callback
triggered a GC, the next callback's function could be freed or moved, and
calling it segfaulted the page's script thread ("Caught signal 11 in thread
Script#1", in JS::GetNonCCWObjectGlobal). The whole browser went down. Heavy
sites with many timers (e.g. Instagram) hit it within seconds. Fixed by
backporting servo/servo#48548 (vendor/servo-script).

This runs a page that keeps scheduling batches of allocating timers and checks
the browser is still alive afterwards. Works with any Servo shell binary, so it
also shows the bug in unpatched servoshell:

  python3 tests/regressions/timer_gc_crash.py --launch target/release/servoaf
"""

import argparse
import base64
import shlex
import subprocess
import sys
import tempfile
import time

PAGE = """<!doctype html><title>timers</title><script>
let keep = [];
function churn() { let a = []; for (let i = 0; i < 20000; i++) a.push({i, s: 'x' + i, v: new Array(8)}); keep[Math.random() * 64 | 0] = a; }
function round() {
  // Fresh closures live in the GC nursery, which a minor GC moves.
  for (let k = 0; k < 40; k++) setTimeout(() => churn(), 0);
  setTimeout(() => round(), 5);
}
for (let k = 0; k < 4; k++) round();
</script>"""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--launch", required=True, metavar="BINARY", help="servoaf or any servoshell")
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--runs", type=int, default=3)
    args = parser.parse_args()

    url = "data:text/html;base64," + base64.b64encode(PAGE.encode()).decode()
    for run in range(1, args.runs + 1):
        with tempfile.TemporaryDirectory(prefix="servoaf-regress-") as profile:
            argv = shlex.split(args.launch) + ["--headless", f"--config-dir={profile}", url]
            if "servoaf" in args.launch:
                argv.insert(-1, "--mcp-port=0")
            log = tempfile.TemporaryFile()
            proc = subprocess.Popen(argv, stdout=log, stderr=log)
            deadline = time.time() + args.seconds
            while time.time() < deadline and proc.poll() is None:
                time.sleep(0.5)
            code = proc.poll()
            if code is None:
                proc.terminate()
                try:
                    proc.wait(10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                print(f"run {run}: alive after {args.seconds:.0f}s")
                continue
            log.seek(0)
            tail = log.read().decode(errors="replace").splitlines()[-15:]
            print(f"FAIL: run {run}: browser exited with {code} after "
                  f"{args.seconds - (deadline - time.time()):.1f}s\n  " + "\n  ".join(tail))
            sys.exit(1)
    print("PASS: timers survived GC")


if __name__ == "__main__":
    main()
