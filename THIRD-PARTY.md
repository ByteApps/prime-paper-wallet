# Third-party libraries

Direct dependencies of this app and its `wallet-core` library. The complete transitive list (with exact versions) is pinned in [`Cargo.lock`](Cargo.lock).

## Rust crates

| Library | Version | License | Used for |
|---|---|---|---|
| [k256](https://crates.io/crates/k256) | 0.13 | Apache-2.0 OR MIT | secp256k1 math, including the BIP341 taproot tweak |
| [sha2](https://crates.io/crates/sha2) | 0.10 | MIT OR Apache-2.0 | SHA-256 hashing |
| [ripemd](https://crates.io/crates/ripemd) | 0.1 | MIT OR Apache-2.0 | RIPEMD-160 (P2WPKH pubkey hash) |
| [hkdf](https://crates.io/crates/hkdf) | 0.12 | MIT OR Apache-2.0 | Deterministic backup-key derivation |
| [bs58](https://crates.io/crates/bs58) | 0.5 | MIT OR Apache-2.0 | WIF (Base58Check) encoding |
| [bech32](https://crates.io/crates/bech32) | 0.11 | MIT | SegWit/Taproot addresses (BIP173/BIP350) |
| [getrandom](https://crates.io/crates/getrandom) | 0.2 | MIT OR Apache-2.0 | Entropy source (see vendored override below) |
| [qrcode](https://crates.io/crates/qrcode) | 0.12 | MIT OR Apache-2.0 | QR code generation for the bill |
| [ab_glyph](https://crates.io/crates/ab_glyph) | 0.2 | Apache-2.0 | Font rasterization for bill text |
| [image](https://crates.io/crates/image) | 0.25 | MIT OR Apache-2.0 | PNG composition (PNG feature only) |
| [serde](https://crates.io/crates/serde) / [serde_json](https://crates.io/crates/serde_json) | 1 | MIT OR Apache-2.0 | Backup JSON and gift metadata |
| [log](https://crates.io/crates/log) | 0.4 | MIT OR Apache-2.0 | Logging facade |
| [hex](https://crates.io/crates/hex) (dev) | 0.4 | MIT OR Apache-2.0 | Test vectors |
| [rqrr](https://crates.io/crates/rqrr) (dev) | 0.10 | (MIT OR Apache-2.0) AND ISC | QR round-trip decoding in tests |

## Vendored code

| Component | Origin | Role |
|---|---|---|
| `vendor/getrandom/` | KeyOS source (getrandom 0.2 fork) | Entropy override: hardware TRNG server on KeyOS builds, stock behavior on host |
| `vendor/security-api/` | KeyOS v1.2.1 source, adapted to SDK 0.4.0 conventions | `os/security` API client (`GetAppSeed`) |

## Fonts & artwork

| Asset | License / credit |
|---|---|
| DejaVu Sans Mono & DejaVu Sans Condensed (`wallet-core/assets/`) | Bitstream Vera / DejaVu license — see `wallet-core/assets/DEJAVU-LICENSE` |
| "Satoshi bill" artwork (`wallet-core/assets/bill_template.png`) | [u/CoinCult](https://www.reddit.com/r/Bitcoin/comments/20rml2/heres_a_paper_wallet_enjoy_or_not_im_bored/) (via the bitcoin-gift-paper-wallet web app) |

## Foundation SDK / KeyOS platform

Provided by the installed Foundation SDK (path dependencies, not crates.io):

| Component | Role |
|---|---|
| `server` (KeyOS) | App runtime, KeyOS service messaging, filesystem API |
| `xous-api-log` | Log output to the KeyOS log server |
| `slint-keyos-platform` (+ `-build`) | [Slint](https://slint.dev) UI runtime, QR rendering, and build integration for KeyOS |
| `foundation-themes` | Design tokens and light/dark theming |

The Slint UI toolkit itself is licensed under GPL-3.0-only OR the Slint
Royalty-free / commercial licenses. **This app elects the GPL**, which is why
it is GPL-3.0-or-later. That is not a free choice: section 3 of the Slint
Royalty-free license excludes embedded systems, and a Passport Prime is one, so
on-device the GPL is the only option that costs nothing. KeyOS's own API crates
(`server`, `fs`, `crypto`, `security`, ...) are GPL-3.0-or-later as well. Taking
this app closed-source would require a paid Slint license *and* a resolution of
the KeyOS side.
