# Vendored bot-detection libraries

These are unmodified distribution builds of open-source, non-CAPTCHA
bot/automation detectors, committed so the suite runs offline and
reproducibly. Each directory holds the single file the detector page imports,
the upstream `LICENSE`, and a `VERSION`. To refresh, download the same npm
tarball, verify its sha256, and re-extract the listed file.

| Library | Version | License | File used | npm tarball |
|---|---|---|---|---|
| Fingerprint BotD | 2.0.0 | MIT | `botd/botd.esm.js` (`package/dist/botd.esm.js`) | `@fingerprintjs/botd` |
| fpscanner | 1.0.8 | MIT | `fpscanner/fpScanner.es.js` (`package/dist/fpScanner.es.js`) | `fpscanner` |
| isbot | 5.2.2 | Unlicense | `isbot/isbot.mjs` (`package/index.mjs`) | `isbot` |

Tarball sha256 (the files here are extracted verbatim from these):

```
0bd58c59ee27d9bd6e355a2803a76b81bcdb7683b031ac7b6200346ee4a84af7  @fingerprintjs/botd 2.0.0
3a974c234805746f81791a2ec320db541829221c8035146706c44ec744c85e5d  fpscanner 1.0.8
5782d736fa7c2a837eeb5f015fd48470e77ac136fb0e7425059b11e1a5dca883  isbot 5.2.2
```

Refresh example:

```sh
curl -s https://registry.npmjs.org/@fingerprintjs/botd/-/botd-2.0.0.tgz -o botd.tgz
sha256sum botd.tgz   # must match the table above
tar -xzOf botd.tgz package/dist/botd.esm.js > botd/botd.esm.js
tar -xzOf botd.tgz package/LICENSE          > botd/LICENSE
```

How each is used, from `../page/index.html`:

- **BotD** — `load({monitoring:false})` then `detect()` → `{bot, botKind}`.
  Monitoring is off so it makes no network call.
- **fpscanner** — `new FingerprintScanner().collectFingerprint({encrypt:false})`
  → `fastBotDetection` plus per-signal `fastBotDetectionDetails`.
- **isbot** — `isbot(navigator.userAgent)` over the user-agent string.
