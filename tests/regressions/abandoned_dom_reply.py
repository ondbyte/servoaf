#!/usr/bin/env python3
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
"""Regression test: an abandoned DOM request must not crash the page.

servoaf reads the DOM through Servo's WebDriver script commands. Servo's
script thread unwrap()s the send of every reply, so if servoaf stopped
waiting and closed the reply channel, a late answer panicked the script
thread ("webdriver_handlers.rs:855 ... Disconnected") and killed the tab.

This launches servoaf with a 2 s tool timeout, cancels get-clickables on a
heavy page mid-way several times, waits for the late replies, and checks the
page is still alive.

  python3 tests/regressions/abandoned_dom_reply.py --launch target/release/servoaf
"""

import argparse
import base64
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "scripts"))
from mcp_smoke_test import McpClient, wait_for_server  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--launch", required=True, metavar="SERVOAF")
    parser.add_argument("--port", type=int, default=9241)
    args = parser.parse_args()

    url = f"http://127.0.0.1:{args.port}/mcp"
    with tempfile.TemporaryDirectory(prefix="servoaf-regress-") as profile:
        log = tempfile.TemporaryFile()
        proc = subprocess.Popen([args.launch, "--headless", f"--mcp-port={args.port}",
                                 f"--user-data-dir={profile}", "--tool-timeout", "2"],
                                stdout=subprocess.DEVNULL, stderr=log)
        try:
            wait_for_server(url, 60)
            client = McpClient(url)
            client.request("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                                          "clientInfo": {"name": "regress", "version": "1"}})
            rows = "".join(f'<div style="cursor:pointer">item {i} <a href="#{i}">l{i}</a></div>'
                           for i in range(6000))
            page = "data:text/html;base64," + base64.b64encode(
                ("<!doctype html><title>heavy</title>" + rows).encode()).decode()
            tab = client.call_json("create-tab", url=page, wait_for_load=False)["tab_id"]
            time.sleep(6)
            cancelled = 0
            for _ in range(3):
                result = client.request("tools/call", {"name": "get-clickables", "arguments": {
                    "tab_id": tab, "include_offscreen": True, "max_count": 5000}})
                cancelled += "timed out" in result["content"][0]["text"]
            time.sleep(8)  # let the script thread answer the abandoned requests
            result = client.request("tools/call", {"name": "get-page-text", "arguments": {"tab_id": tab}})
            text = result["content"][0]["text"]
            log.seek(0)
            panicked = b"panicked" in log.read()
            print(f"cancelled calls: {cancelled}/3")
            ok = cancelled > 0 and not result.get("isError") and text.startswith("item 0") and not panicked
            if not cancelled:
                print("FAIL: no call was cancelled; the test did not exercise the bug")
            elif not ok:
                print(f"FAIL: page died after abandoned DOM requests (panicked={panicked}): {text[:80]!r}")
            else:
                print("PASS: page survives abandoned DOM requests")
            return 0 if ok else 1
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()


if __name__ == "__main__":
    sys.exit(main())
