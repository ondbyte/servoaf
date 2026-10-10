# Copyright 2026 The servoaf Authors
# SPDX-License-Identifier: MPL-2.0
"""Capture servoaf's TLS ClientHello and compute its JA4 fingerprint.

JA4 (FoxIO, https://github.com/FoxIO-LLC/ja4) is the current industry-standard
TLS client fingerprint. It is derived entirely from the ClientHello, so this
probe listens on a raw socket, lets servoaf start a TLS handshake against it
(the handshake never completes — only the ClientHello is needed), parses the
record and computes JA4.

servoaf drives its network stack through Servo, which uses rustls. rustls's
ClientHello (cipher/extension/group ordering, GREASE, ALPN) differs from the
NSS stack that Firefox uses, so the JA4 does not match the Firefox our user
agent claims. Changing that requires modifying Servo's `net` crate / rustls
configuration, which is outside the servoaf workspace — see the suite README.

Standalone use:
    python3 ja4_probe.py --port 8899        # then point a TLS client at it
"""

import hashlib
import socket

GREASE = {0x0a0a, 0x1a1a, 0x2a2a, 0x3a3a, 0x4a4a, 0x5a5a, 0x6a6a, 0x7a7a,
          0x8a8a, 0x9a9a, 0xaaaa, 0xbaba, 0xcaca, 0xdada, 0xeaea, 0xfafa}


def is_grease(value):
    return value in GREASE


class ParseError(Exception):
    pass


def _u16(b, i):
    return (b[i] << 8) | b[i + 1]


def read_client_hello(sock, timeout=10):
    """Reads one TLS record and returns the ClientHello handshake body bytes."""
    sock.settimeout(timeout)
    header = b""
    while len(header) < 5:
        chunk = sock.recv(5 - len(header))
        if not chunk:
            raise ParseError("connection closed before a full TLS record header")
        header += chunk
    if header[0] != 0x16:
        raise ParseError(f"not a TLS handshake record (first byte 0x{header[0]:02x})")
    length = _u16(header, 3)
    body = b""
    while len(body) < length:
        chunk = sock.recv(length - len(body))
        if not chunk:
            break
        body += chunk
    if not body or body[0] != 0x01:
        raise ParseError("record is not a ClientHello")
    return body


def parse_client_hello(body):
    """Parses a ClientHello handshake body into the fields JA4 needs."""
    # handshake header: type(1) length(3)
    i = 4
    legacy_version = _u16(body, i); i += 2
    i += 32  # random
    sid_len = body[i]; i += 1 + sid_len
    cs_len = _u16(body, i); i += 2
    ciphers = [_u16(body, i + j) for j in range(0, cs_len, 2)]
    i += cs_len
    comp_len = body[i]; i += 1 + comp_len

    extensions, sig_algs, alpns = [], [], []
    supported_versions, has_sni = [], False
    if i + 2 <= len(body):
        ext_total = _u16(body, i); i += 2
        end = i + ext_total
        while i + 4 <= end:
            etype = _u16(body, i); elen = _u16(body, i + 2); i += 4
            edata = body[i:i + elen]; i += elen
            extensions.append(etype)
            if etype == 0x0000:
                has_sni = True
            elif etype == 0x000d and len(edata) >= 2:  # signature_algorithms
                n = _u16(edata, 0)
                sig_algs = [_u16(edata, 2 + j) for j in range(0, n, 2)]
            elif etype == 0x0010 and len(edata) >= 2:  # ALPN
                n = _u16(edata, 0); j = 2
                while j < 2 + n and j < len(edata):
                    plen = edata[j]; j += 1
                    alpns.append(edata[j:j + plen].decode("latin1")); j += plen
            elif etype == 0x002b and len(edata) >= 1:  # supported_versions
                n = edata[0]
                supported_versions = [_u16(edata, 1 + j) for j in range(0, n, 2)]
    return {
        "legacy_version": legacy_version,
        "ciphers": ciphers,
        "extensions": extensions,
        "sig_algs": sig_algs,
        "alpns": alpns,
        "supported_versions": supported_versions,
        "has_sni": has_sni,
    }


def _version_code(hello):
    best = 0
    for v in hello["supported_versions"]:
        if not is_grease(v):
            best = max(best, v)
    best = best or hello["legacy_version"]
    return {0x0304: "13", 0x0303: "12", 0x0302: "11", 0x0301: "10", 0x0300: "s3"}.get(best, "00")


def _sha12(text):
    return hashlib.sha256(text.encode()).hexdigest()[:12] if text else "000000000000"


def compute_ja4(hello):
    ciphers = [c for c in hello["ciphers"] if not is_grease(c)]
    exts = [e for e in hello["extensions"] if not is_grease(e)]
    sigs = [s for s in hello["sig_algs"] if not is_grease(s)]

    version = _version_code(hello)
    sni = "d" if hello["has_sni"] else "i"
    nc = min(len(ciphers), 99)
    ne = min(len(exts), 99)
    alpn = hello["alpns"][0] if hello["alpns"] else ""
    alpn_code = (alpn[0] + alpn[-1]) if alpn else "00"
    ja4_a = f"t{version}{sni}{nc:02d}{ne:02d}{alpn_code}"

    ja4_b = _sha12(",".join(f"{c:04x}" for c in sorted(ciphers)))

    # JA4_c excludes SNI (0x0000) and ALPN (0x0010) from the extension list,
    # which is sorted; signature algorithms keep their original order.
    exts_c = sorted(e for e in exts if e not in (0x0000, 0x0010))
    ext_str = ",".join(f"{e:04x}" for e in exts_c)
    sig_str = ",".join(f"{s:04x}" for s in sigs)
    ja4_c = _sha12(f"{ext_str}_{sig_str}" if sig_str else ext_str)

    return f"{ja4_a}_{ja4_b}_{ja4_c}"


def assess(hello):
    """Heuristic: does this ClientHello look like a mainstream browser's?

    Mainstream browsers (Chrome and Firefox alike) inject a GREASE value into
    their cipher and extension lists and advertise a fuller cipher list than a
    lean library default. rustls does neither, so its ClientHello — and the JA4
    derived from it — is identifiably not the Firefox our UA claims. Returns
    (looks_like_browser, reasons)."""
    reasons = []
    has_grease = any(is_grease(c) for c in hello["ciphers"]) or any(is_grease(e) for e in hello["extensions"])
    if not has_grease:
        reasons.append("no GREASE values (mainstream browsers inject GREASE)")
    real_ciphers = [c for c in hello["ciphers"] if not is_grease(c)]
    if len(real_ciphers) < 13:
        reasons.append(f"only {len(real_ciphers)} cipher suites (browsers advertise ~15+)")
    # record_size_limit (0x001c) and renegotiation_info (0xff01) are sent by
    # Firefox but not by rustls.
    if 0x001c not in hello["extensions"]:
        reasons.append("no record_size_limit extension (Firefox sends it)")
    return (len(reasons) == 0, reasons)


def capture(port, on_ready=None, timeout=20):
    """Listens on `port`, calls on_ready() (which should make a client connect),
    and returns (ja4, hello_fields)."""
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", port))
    server.listen(1)
    server.settimeout(timeout)
    try:
        if on_ready:
            on_ready(port)
        conn, _ = server.accept()
        try:
            body = read_client_hello(conn)
        finally:
            conn.close()
    finally:
        server.close()
    hello = parse_client_hello(body)
    return compute_ja4(hello), hello


def main():
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8899)
    args = parser.parse_args()
    print(f"listening on 127.0.0.1:{args.port}; point a TLS client at it...")
    ja4, hello = capture(args.port, timeout=60)
    print("JA4:", ja4)
    print("TLS version:", _version_code(hello))
    print("ciphers (GREASE removed):", len([c for c in hello["ciphers"] if not is_grease(c)]))
    print("extensions (GREASE removed):", len([e for e in hello["extensions"] if not is_grease(e)]))
    print("ALPN:", hello["alpns"])


if __name__ == "__main__":
    main()
