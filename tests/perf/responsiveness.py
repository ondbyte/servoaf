#!/usr/bin/env python3
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
"""Responsiveness benchmark: does servoaf react to a person as fast as Servo?

Opens a heavy single-page-app stand-in (tests/perf/page: a ~2 MB script
bundle, 300 x LOAD cards re-rendered every 100 ms, 4 x LOAD timers, a failing
WebSocket and failing fetch retries; --load 5 saturates a 4-core machine) in each browser given with --browser, types keys into
it with xdotool like a person would, and reports:

  fps        requestAnimationFrame callbacks per second
  lag        event-loop lag: how late a 50 ms setTimeout fires (p50/p95, ms)
  key        time from the X key press to the page's keydown handler (p50/p95/max, ms)
  lost       key presses the page never saw
  cpu        CPU used by the browser and its child processes (% of one core)
  crashed    runs in which the browser died

Every browser gets the same page, window and key sequence, so the rows are
directly comparable. Run under an X server (or let this start Xvfb):

  python3 tests/perf/responsiveness.py \\
      --browser "servoaf=target/release/servoaf --mcp-port 0" \\
      --browser "servoaf+mcp=target/release/servoaf" \\
      --browser "upstream=path/to/servoshell"

With --max-ratio R the run fails if a browser's key latency p95 or cpu is more
than R times the first browser's (use upstream servoshell first as baseline).
"""

import argparse
import http.server
import json
import os
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time

PAGE_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "page")


def make_bundle(size=2_000_000):
    """A big, deterministic script: many small functions, like a minified app bundle."""
    rng = random.Random(7)
    parts, total, i = [], 0, 0
    while total < size:
        a, b = rng.randrange(1, 999), rng.randrange(1, 999)
        part = (f"function f{i}(x){{var y=x*{a}+{b};for(var k=0;k<3;k++)y=(y^{a})%{b + 7};"
                f"return {{id:{i},v:y,s:'item-{i}-{a}-{b}'}}}}\n")
        parts.append(part)
        total += len(part)
        i += 1
    parts.append(f"function bundleChecksum(){{var s=0;for(var i=0;i<{i};i+=97)s+=window['f'+i](i).v;return s}}\n")
    return "".join(parts).encode()


class Reports:
    def __init__(self):
        self.lock = threading.Lock()
        self.items = []

    def add(self, item):
        with self.lock:
            self.items.append(item)

    def take(self):
        with self.lock:
            items, self.items = self.items, []
        return items


def serve(reports, bundle):
    class Handler(http.server.SimpleHTTPRequestHandler):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, directory=PAGE_DIR, **kwargs)

        def log_message(self, *args):
            pass

        def do_GET(self):
            if self.path == "/bundle.js":
                self.send_response(200)
                self.send_header("Content-Type", "text/javascript")
                self.send_header("Content-Length", str(len(bundle)))
                self.end_headers()
                self.wfile.write(bundle)
            elif self.path.startswith("/api/"):
                self.send_error(401)
            else:
                super().do_GET()

        def do_POST(self):
            body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
            try:
                reports.add(json.loads(body))
            except ValueError:
                pass
            self.send_response(204)
            self.end_headers()

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def process_tree_cpu(pid):
    """utime+stime (seconds) of pid and all its descendants."""
    ticks = os.sysconf("SC_CLK_TCK")
    children = {}
    for entry in os.listdir("/proc"):
        if entry.isdigit():
            try:
                with open(f"/proc/{entry}/stat") as f:
                    fields = f.read().rsplit(")", 1)[1].split()
                children.setdefault(int(fields[1]), []).append((int(entry), fields))
            except OSError:
                continue
    total, stack = 0.0, [pid]
    try:
        with open(f"/proc/{pid}/stat") as f:
            fields = f.read().rsplit(")", 1)[1].split()
            total += (int(fields[11]) + int(fields[12])) / ticks
    except OSError:
        return 0.0
    while stack:
        for child, fields in children.get(stack.pop(), []):
            total += (int(fields[11]) + int(fields[12])) / ticks
            stack.append(child)
    return total


def percentile(values, q):
    if not values:
        return float("nan")
    values = sorted(values)
    return values[min(len(values) - 1, int(round(q * (len(values) - 1))))]


def xdotool(*args, env):
    subprocess.run(["xdotool", *args], env=env, check=False,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def run_browser(name, command, url, args, env, reports):
    profile = tempfile.mkdtemp(prefix="servoaf-perf-")
    argv = shlex.split(command) + [f"--config-dir={profile}", f"--window-size={args.window}", url]
    log = tempfile.TemporaryFile()
    proc = subprocess.Popen(argv, env=env, stdout=log, stderr=log)
    try:
        # Wait for the page's first report (it loaded and runs).
        deadline = time.time() + args.load_timeout
        reports.take()
        while time.time() < deadline and not reports.take():
            if proc.poll() is not None:
                raise RuntimeError(f"{name} exited with {proc.returncode}")
            time.sleep(0.2)
        else:
            if time.time() >= deadline:
                raise RuntimeError(f"{name}: page did not load in {args.load_timeout}s")
        time.sleep(args.warmup)
        # Focus the window (there is no window manager under Xvfb), then
        # click into the page like a person.
        windows = subprocess.run(["xdotool", "search", "--pid", str(proc.pid)], env=env,
                                 capture_output=True, text=True).stdout.split()
        if not windows:  # launched through a wrapper (gdb, perf, ...)
            windows = subprocess.run(["xdotool", "search", "--name", "servoaf responsiveness"], env=env,
                                     capture_output=True, text=True).stdout.split()
        if windows:
            xdotool("windowfocus", "--sync", windows[-1], env=env)
        width, height = (int(v) for v in args.window.split("x"))
        xdotool("mousemove", str(width // 2), str(height // 2), env=env)
        xdotool("click", "1", env=env)
        time.sleep(1)
        reports.take()

        cpu_start, wall_start = process_tree_cpu(proc.pid), time.time()
        sent = []
        letters = "abcdefghijklmnopqrstuvwxyz"
        for i in range(args.keys):
            key = letters[i % len(letters)]
            sent.append((key, time.time() * 1000))
            xdotool("key", key, env=env)
            time.sleep(args.key_interval)
        time.sleep(max(0.0, args.duration - args.keys * args.key_interval))
        time.sleep(2.5)  # last report
        cpu = (process_tree_cpu(proc.pid) - cpu_start) / (time.time() - wall_start) * 100
        if proc.poll() is not None:
            raise RuntimeError(f"{name} exited with {proc.returncode} during the run")
    except RuntimeError:
        log.seek(0)
        tail = log.read().decode(errors="replace").splitlines()[-60:]
        print(f"--- {name} output (last lines) ---\n" + "\n".join(tail), file=sys.stderr)
        raise
    finally:
        proc.terminate()
        try:
            proc.wait(10)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(profile, ignore_errors=True)

    items = reports.take()
    fps = [item["frames"] for item in items]
    lags = [lag for item in items for lag in item["lags"]]
    received = [key for item in items for key in item["keys"]]
    latencies, cursor = [], 0
    for key, t_sent in sent:
        for j in range(cursor, len(received)):
            if received[j]["key"] == key and received[j]["t"] >= t_sent - 5:
                latencies.append(max(0.0, received[j]["t"] - t_sent))
                cursor = j + 1
                break
    return {
        "name": name,
        "fps": statistics.median(fps) if fps else 0,
        "lag_p50": percentile(lags, 0.5), "lag_p95": percentile(lags, 0.95),
        "key_p50": percentile(latencies, 0.5), "key_p95": percentile(latencies, 0.95),
        "key_max": max(latencies) if latencies else float("nan"),
        "lost": len(sent) - len(latencies),
        "cpu": cpu,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--browser", action="append", required=True, metavar="NAME=COMMAND")
    parser.add_argument("--rounds", type=int, default=1, help="repeat each browser, interleaved")
    parser.add_argument("--load", type=int, default=1, help="page workload scale (0 = idle, 1 = busy app)")
    parser.add_argument("--keys", type=int, default=40)
    parser.add_argument("--key-interval", type=float, default=0.25)
    parser.add_argument("--duration", type=float, default=15, help="seconds measured per run")
    parser.add_argument("--warmup", type=float, default=4)
    parser.add_argument("--load-timeout", type=float, default=90)
    parser.add_argument("--window", default="1280x800")
    parser.add_argument("--max-ratio", type=float, help="fail if key p95 or cpu exceeds R x the first browser")
    parser.add_argument("--json", help="also write the results here")
    args = parser.parse_args()

    env = dict(os.environ)
    xvfb = None
    if not env.get("DISPLAY"):
        env["DISPLAY"] = ":97"
        xvfb = subprocess.Popen(["Xvfb", ":97", "-screen", "0", "1920x1080x24", "-nolisten", "tcp"],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)

    reports = Reports()
    server = serve(reports, make_bundle())
    url = f"http://127.0.0.1:{server.server_address[1]}/index.html?load={args.load}"
    browsers = [spec.split("=", 1) for spec in args.browser]
    results = {name: [] for name, _ in browsers}
    crashes = {name: 0 for name, _ in browsers}
    try:
        for _ in range(args.rounds):
            for name, command in browsers:
                try:
                    result = run_browser(name, command, url, args, env, reports)
                except RuntimeError as error:
                    print(f"  {name}: CRASHED: {error}", flush=True)
                    crashes[name] += 1
                    continue
                results[name].append(result)
                print(f"  {name}: {json.dumps({k: round(v, 1) if isinstance(v, float) else v for k, v in result.items()})}",
                      flush=True)
    finally:
        server.shutdown()
        if xvfb:
            xvfb.terminate()

    def merged(name):
        runs = results[name]
        keys = ("fps", "lag_p50", "lag_p95", "key_p50", "key_p95", "key_max", "lost", "cpu")
        row = {key: statistics.median(run[key] for run in runs) if runs else float("nan") for key in keys}
        row["crashes"] = crashes[name]
        return row

    summary = {name: merged(name) for name, _ in browsers}
    print()
    print(f"{'browser':<16}{'fps':>6}{'lag p50':>9}{'lag p95':>9}{'key p50':>9}{'key p95':>9}"
          f"{'key max':>9}{'lost':>6}{'cpu %':>8}{'crashed':>9}")
    for name, row in summary.items():
        print(f"{name:<16}{row['fps']:>6.0f}{row['lag_p50']:>9.1f}{row['lag_p95']:>9.1f}"
              f"{row['key_p50']:>9.1f}{row['key_p95']:>9.1f}{row['key_max']:>9.1f}"
              f"{row['lost']:>6.0f}{row['cpu']:>8.0f}{row['crashes']:>9}")
    if args.json:
        with open(args.json, "w") as f:
            json.dump({"runs": results, "summary": summary}, f, indent=2)

    if args.max_ratio:
        base_name = browsers[0][0]
        base = summary[base_name]
        failed = False
        for name, row in summary.items():
            for key in ("key_p95", "cpu"):
                # Small absolute differences are noise, not regressions.
                floor = 20 if key == "key_p95" else 10
                if row[key] > max(base[key] * args.max_ratio, base[key] + floor):
                    print(f"FAIL: {name} {key} {row[key]:.1f} vs {base_name} {base[key]:.1f}")
                    failed = True
            if row["crashes"] > base["crashes"]:
                print(f"FAIL: {name} crashed {row['crashes']} times")
                failed = True
            if row["lost"] > base["lost"]:
                print(f"FAIL: {name} lost {row['lost']:.0f} keys vs {base_name} {base['lost']:.0f}")
                failed = True
        sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
