#!/usr/bin/env python3
# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
"""End-to-end smoke test for a running servoaf instance.

Talks MCP (Streamable HTTP, JSON responses) to the browser and exercises every
tool against a self-contained test page, checking that synthetic input arrives
as trusted events and that the browser does not look automated.

  scripts/mcp_smoke_test.py --url http://127.0.0.1:9223/mcp
  scripts/mcp_smoke_test.py --launch target/release/servoaf      # headless

Only the Python standard library is used.
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

TEST_PAGE = """<!DOCTYPE html>
<html><head><title>servoaf smoke</title>
<style>body{font:16px sans-serif;margin:40px} #spacer{height:3000px}</style>
</head><body>
<h1>servoaf smoke test</h1>
<button id="btn" style="width:160px;height:48px">Press me</button>
<input id="field" placeholder="Type here" style="width:300px">
<a href="#anchor" id="lnk">A link</a>
<button id="dbl" style="width:160px;height:40px">Double me</button>
<button id="ask" style="width:160px;height:40px">Ask</button>
<select id="sel"><option>one</option><option>two</option><option>three</option></select>
<div id="fake" style="cursor:pointer;width:120px;height:30px;background:#eee">Fake button</div>
<div id="log"></div>
<div id="spacer"></div>
<script>
  const state = {moves: 0, clicks: [], context: 0, typed: '', dbl: 0, answer: null, sel: ''};
  document.getElementById('dbl').addEventListener('dblclick', e => { if (e.isTrusted) state.dbl++; });
  document.getElementById('ask').addEventListener('click', () => { state.answer = confirm('Proceed?'); });
  document.getElementById('sel').addEventListener('change', e => { state.sel = e.target.value; });
  addEventListener('mousemove', e => { if (e.isTrusted) state.moves++; });
  document.getElementById('btn').addEventListener('click', e => {
    state.clicks.push({trusted: e.isTrusted, x: e.clientX, y: e.clientY});
  });
  addEventListener('contextmenu', e => { state.context++; e.preventDefault(); });
  document.getElementById('field').addEventListener('input', e => {
    state.typed = e.target.value + '|' + e.isTrusted;
  });
  setInterval(() => {
    document.getElementById('log').textContent =
      'STATE ' + JSON.stringify(state) + ' WEBDRIVER ' + navigator.webdriver +
      ' SCROLL ' + Math.round(scrollY);
  }, 50);
</script>
</body></html>"""


class McpClient:
    def __init__(self, url, token=None):
        self.url = url
        self.token = token
        self.session = None
        self.next_id = 1

    def _post(self, payload):
        data = json.dumps(payload).encode()
        req = urllib.request.Request(self.url, data=data, method="POST")
        req.add_header("Content-Type", "application/json")
        req.add_header("Accept", "application/json, text/event-stream")
        if self.token:
            req.add_header("Authorization", "Bearer " + self.token)
        if self.session:
            req.add_header("Mcp-Session-Id", self.session)
        with urllib.request.urlopen(req, timeout=90) as resp:
            if resp.headers.get("Mcp-Session-Id"):
                self.session = resp.headers["Mcp-Session-Id"]
            body = resp.read()
            return json.loads(body) if body else None

    def request(self, method, params=None):
        msg = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        self.next_id += 1
        if params is not None:
            msg["params"] = params
        resp = self._post(msg)
        if "error" in resp:
            raise RuntimeError(f"{method}: {resp['error']}")
        return resp["result"]

    def notify(self, method):
        self._post({"jsonrpc": "2.0", "method": method})

    def call(self, name, **arguments):
        result = self.request("tools/call",
                              {"name": name, "arguments": arguments})
        if result.get("isError"):
            text = " ".join(c.get("text", "") for c in result["content"])
            raise RuntimeError(f"{name} failed: {text}")
        return result

    def call_json(self, name, **arguments):
        result = self.call(name, **arguments)
        if "structuredContent" in result:
            return result["structuredContent"]
        return json.loads(result["content"][0]["text"])

    def text(self, name, **arguments):
        return self.call(name, **arguments)["content"][0]["text"]


class Checks:
    def __init__(self):
        self.failed = 0

    def check(self, condition, message):
        print(("  ok   " if condition else "  FAIL ") + message)
        if not condition:
            self.failed += 1


def page_state(client, tab_id):
    text = client.text("get-page-text", tab_id=tab_id)
    for line in text.splitlines():
        if line.startswith("STATE "):
            state_json, rest = line[6:].split(" WEBDRIVER ")
            webdriver, scroll = rest.split(" SCROLL ")
            return json.loads(state_json), webdriver.strip(), int(scroll)
    raise RuntimeError("state line not found in page text:\n" + text[:500])


def wait_for_server(url, timeout):
    base = urllib.parse.urljoin(url, "/")
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            urllib.request.urlopen(base, timeout=2).read()
            return
        except urllib.error.HTTPError:
            return  # Up, but e.g. asking for a token.
        except (urllib.error.URLError, OSError):
            time.sleep(0.5)
    raise RuntimeError(f"MCP server at {url} did not come up")


def run(client):
    c = Checks()

    print("protocol")
    init = client.request("initialize", {
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "servoaf-smoke", "version": "1"},
    })
    client.notify("notifications/initialized")
    c.check(init["serverInfo"]["name"] == "servoaf", "initialize")
    names = {t["name"] for t in client.request("tools/list")["tools"]}
    expected = {"get-whole-page-as-html", "get-clickables", "get-screen-shot",
                "move-cursor", "click", "rightclick", "get-tabs", "create-tab",
                "close-tab", "navigate", "scroll", "type-text", "press-key",
                "double-click", "get-page-text", "go-back", "go-forward", "reload",
                "wait-for-load", "activate-tab", "get-cursor-position",
                "get-dialogs", "respond-dialog"}
    c.check(expected <= names, f"tools/list has {sorted(expected - names) or 'all core tools'}")

    print("tabs")
    url = "data:text/html;base64," + base64.b64encode(TEST_PAGE.encode()).decode()
    tab = client.call_json("create-tab", url=url)
    tab_id = tab["tab_id"]
    c.check(tab["title"] == "servoaf smoke", "create-tab loads the test page")
    windows = client.call_json("get-tabs")["windows"]
    ids = [t["tab_id"] for w in windows for t in w["tabs"]]
    c.check(tab_id in ids, "get-tabs lists the new tab")

    print("page")
    html = client.text("get-whole-page-as-html", tab_id=tab_id)
    c.check("<h1>servoaf smoke test</h1>" in html, "get-whole-page-as-html")
    clickables = client.call_json("get-clickables", tab_id=tab_id)["clickables"]
    button = next((x for x in clickables if x.get("dom_id") == "btn"), None)
    c.check(button is not None, "get-clickables finds the button")
    c.check(any(x.get("dom_id") == "field" for x in clickables),
            "get-clickables finds the input")
    c.check(any(x.get("dom_id") == "fake" for x in clickables),
            "get-clickables finds the cursor:pointer widget")
    c.check(button is not None and button.get("text") == "Press me",
            "clickable label is the button text")

    shot = client.call("get-screen-shot", tab_id=tab_id)
    image = next(x for x in shot["content"] if x["type"] == "image")
    png = base64.b64decode(image["data"])
    c.check(png[:8] == b"\x89PNG\r\n\x1a\n", "get-screen-shot returns a PNG")
    shot = client.call("get-screen-shot", tab_id=tab_id, format="jpeg", scale=0.5,
                       x=0, y=0, width=200, height=100)
    image = next(x for x in shot["content"] if x["type"] == "image")
    c.check(base64.b64decode(image["data"])[:2] == b"\xff\xd8", "clipped JPEG screenshot")

    print("input")
    client.call("move-cursor", tab_id=tab_id, x=400, y=300)
    client.call("click", tab_id=tab_id, clickable_id=button["id"])
    client.call("rightclick", tab_id=tab_id, x=500, y=400)
    field = next(x for x in clickables if x.get("dom_id") == "field")
    client.call("click", tab_id=tab_id, clickable_id=field["id"])
    client.call("type-text", tab_id=tab_id, text="Hi there")
    client.call("press-key", tab_id=tab_id, key="Backspace")
    by_id = {x.get("dom_id"): x for x in clickables}
    client.call("double-click", tab_id=tab_id, clickable_id=by_id["dbl"]["id"])
    client.call("click", tab_id=tab_id, clickable_id=by_id["ask"]["id"])
    dialogs = []
    for _ in range(40):
        dialogs = client.call_json("get-dialogs", tab_id=tab_id)["dialogs"]
        if dialogs:
            break
        time.sleep(0.1)
    c.check(len(dialogs) == 1 and dialogs[0]["type"] == "confirm"
            and dialogs[0]["message"] == "Proceed?", "get-dialogs shows the confirm()")
    started = time.time()
    blocked = client.request("tools/call", {"name": "get-page-text", "arguments": {"tab_id": tab_id}})
    c.check(blocked["isError"] and "respond-dialog" in blocked["content"][0]["text"]
            and time.time() - started < 3, "page tools fail fast while a dialog is open")
    if dialogs:
        client.call("respond-dialog", dialog_id=dialogs[0]["dialog_id"], action="accept")
    client.call("click", tab_id=tab_id, clickable_id=by_id["sel"]["id"])
    popups = []
    for _ in range(40):
        popups = [d for d in client.call_json("get-dialogs", tab_id=tab_id)["dialogs"]
                  if d["type"] == "select"]
        if popups:
            break
        time.sleep(0.1)
    c.check(len(popups) == 1 and len(popups[0]["options"]) == 3, "select popup is listed")
    if popups:
        third = popups[0]["options"][2]["option_id"]
        client.call("respond-dialog", dialog_id=popups[0]["dialog_id"], option_ids=[third])
    pos = client.call_json("get-cursor-position", tab_id=tab_id)
    c.check(pos["viewport"]["width"] > 0, "get-cursor-position")
    client.call("scroll", tab_id=tab_id, delta_y=600)
    time.sleep(0.8)  # let smooth scrolling and the page's state ticker settle

    state, webdriver, scroll = page_state(client, tab_id)
    c.check(state["moves"] > 10, f"trusted mousemove trail ({state['moves']} events)")
    c.check(len(state["clicks"]) == 1 and state["clicks"][0]["trusted"],
            "button received one trusted click")
    if state["clicks"]:
        bx, by, bw, bh = (button["bounds"][k] for k in ("x", "y", "width", "height"))
        cx, cy = state["clicks"][0]["x"], state["clicks"][0]["y"]
        c.check(bx <= cx <= bx + bw and by <= cy <= by + bh,
                "click landed inside the button")
    c.check(state["context"] == 1, "rightclick opened one contextmenu")
    c.check(state["typed"] == "Hi ther|true", f"typed text is trusted ({state['typed']!r})")
    c.check(webdriver in ("false", "undefined"), f"navigator.webdriver is not set ({webdriver})")
    c.check(state["dbl"] == 1, f"double-click fired one trusted dblclick ({state['dbl']})")
    c.check(state["answer"] is True, "confirm() was accepted")
    c.check(state["sel"] == "three", f"select popup choice applied ({state['sel']!r})")
    c.check(scroll > 300, f"wheel scrolled the page ({scroll}px)")

    print("navigation")
    nav = client.call_json("navigate", tab_id=tab_id, url="about:blank")
    c.check(nav["url"] == "about:blank" and nav.get("load_complete"), "navigate")
    back = client.call_json("go-back", tab_id=tab_id)
    c.check(back["title"] == "servoaf smoke", f"go-back ({back['url'][:40]})")
    client.call("press-key", tab_id=tab_id, key="Control+a")
    second = client.call_json("create-tab", url="about:blank", foreground=False)
    c.check(not second["active"], "create-tab in the background")
    client.call("activate-tab", tab_id=second["tab_id"])
    active = [t for w in client.call_json("get-tabs")["windows"] for t in w["tabs"] if t["active"]]
    c.check([t["tab_id"] for t in active] == [second["tab_id"]], "activate-tab")
    client.call("close-tab", tab_id=second["tab_id"])
    client.call("close-tab", tab_id=tab_id)
    windows = client.call_json("get-tabs")["windows"]
    ids = [t["tab_id"] for w in windows for t in w["tabs"]]
    c.check(tab_id not in ids, "close-tab")

    return c.failed


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--url", default="http://127.0.0.1:9223/mcp")
    parser.add_argument("--token")
    parser.add_argument("--launch", metavar="SERVOAF",
                        help="start this servoaf binary (headless unless --headed)")
    parser.add_argument("--headed", action="store_true",
                        help="with --launch, open a window instead of --headless")
    parser.add_argument("--extra-arg", action="append", default=[],
                        help="extra switch for --launch (repeatable)")
    args = parser.parse_args()

    proc = None
    profile = None
    try:
        if args.launch:
            port = urllib.parse.urlparse(args.url).port or 9223
            profile = tempfile.TemporaryDirectory(prefix="servoaf-smoke-")
            cmd = [args.launch, f"--user-data-dir={profile.name}",
                   f"--mcp-port={port}"] + args.extra_arg
            if not args.headed:
                cmd.append("--headless")
            if args.token:
                cmd.append(f"--mcp-token={args.token}")
            verbose = os.environ.get("SMOKE_VERBOSE")
            proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL,
                                    stderr=None if verbose else subprocess.DEVNULL)
        wait_for_server(args.url, 60)
        failed = run(McpClient(args.url, args.token))
    finally:
        if proc:
            proc.terminate()
            try:
                proc.wait(timeout=15)
            except subprocess.TimeoutExpired:
                proc.kill()
        if profile:
            profile.cleanup()
    print("PASS" if not failed else f"{failed} check(s) FAILED")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
