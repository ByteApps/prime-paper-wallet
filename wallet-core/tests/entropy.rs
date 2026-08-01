//! Entropy/randomness tests for the gift-key generation path.
//!
//! Prompted by a 2026 public disclosure of an RNG failure in a
//! shipped hardware wallet's firmware: a hardware wallet
//! shipped key generation that silently used a deterministic PRNG (bug 1),
//! and a reseed where only 4 of 32 bytes reached the generator, capping it
//! at 2^32 states (bug 2). This file runs the canonical statistical
//! battery (`common/entropy_battery.rs`, shared verbatim across four
//! repos — do not fork it here) against the REAL gift-key path,
//! `wallet_core::keys::generate_private_key()`, plus a key-validity
//! contract on top of it.
//!
//! Structural defenses against bug 1 (statistically undetectable — a
//! fixed-seed CSPRNG is statistically perfect) live in `rng_backend.rs`,
//! not here.

#[path = "common/entropy_battery.rs"]
mod battery;

use battery::controls;
use wallet_core::keys;

/// The real gift-key generator, adapted to the battery's `FnMut(&mut [u8])`
/// shape: one call = one freshly generated, scalar-valid private key.
fn gift_key(out: &mut [u8]) {
    let k = keys::generate_private_key().expect("real entropy source must not fail");
    out.copy_from_slice(&k);
}

fn gift_key32(out: &mut [u8; 32]) {
    let k = keys::generate_private_key().expect("real entropy source must not fail");
    *out = k;
}

/// The same underlying real entropy source `generate_private_key` draws
/// from, exposed as a raw byte filler (not rejection-sampled to a scalar)
/// so the negative controls below have a "good" source to corrupt — the
/// analog of validate.rs's `/dev/urandom` helper for this repo, and
/// literally the code `generate_private_key` calls.
fn raw_entropy(out: &mut [u8]) {
    getrandom::getrandom(out).expect("real entropy source must not fail");
}

// ------------------------- positive: the real gift-key path -------------------------

#[test]
fn real_source_passes_battery() {
    let r = battery::battery_from(32, gift_key);
    println!("{}", r.summary());
    r.assert_ok("generate_private_key()");
}

#[test]
fn real_source_draw_sanity() {
    battery::draw_sanity(10_000, gift_key32).assert_ok("generate_private_key() draws");
}

#[test]
fn real_source_collision_free() {
    let t = std::time::Instant::now();
    let r = battery::collision_freedom(gift_key32);
    println!("collision test took {:?}\n{}", t.elapsed(), r.summary());
    r.assert_ok("generate_private_key() collisions");
}

// ------------------------- negative controls -------------------------
//
// Ported verbatim (in spirit) from the canonical validate.rs harness, so
// this battery is proven to discriminate rather than being silently green.

fn assert_fails(r: &battery::Report, expect: &[&str], what: &str) {
    assert!(!r.passed(), "{what} MUST fail the battery but passed:\n{}", r.summary());
    let failed = r.failed_names();
    for e in expect {
        assert!(
            failed.contains(e),
            "{what} should have tripped `{e}`; tripped {failed:?}\n{}",
            r.summary()
        );
    }
    println!("{what} correctly failed: {failed:?}");
}

#[test]
fn control_zeros_fails() {
    let r = battery::battery_from(32, controls::zeros);
    assert_fails(&r, &["not_degenerate", "monobit", "longest_run", "shannon_entropy"], "all-zero source");
}

#[test]
fn control_counter_fails() {
    let mut c = controls::Counter::default();
    let r = battery::battery_from(8, |o| c.fill(o));
    assert_fails(&r, &["byte_chi_square"], "counter source");
}

#[test]
fn control_truncated_fails() {
    // disclosure bug 2: 4 of every 32 bytes actually filled.
    let mut t = controls::Truncated { inner: raw_entropy, kept: 4 };
    let r = battery::battery_from(32, |o| t.fill(o));
    assert_fails(&r, &["monobit", "shannon_entropy"], "4-of-32-bytes source");
}

#[test]
fn control_stuck_bit_fails() {
    let mut s = controls::StuckBit(raw_entropy);
    let r = battery::battery_from(32, |o| s.fill(o));
    assert_fails(&r, &["bit_position_bias"], "stuck-low-bit source");
}

#[test]
fn control_biased_fails() {
    let mut s = controls::Biased(raw_entropy);
    let r = battery::battery_from(32, |o| s.fill(o));
    assert_fails(&r, &["monobit", "bit_position_bias"], "7-bit masked source");
}

#[test]
fn control_repeating_page_fails() {
    let mut s = controls::RepeatingPage::new(raw_entropy, 4096);
    let r = battery::battery_from(32, |o| s.fill(o));
    assert_fails(&r, &["repeated_blocks"], "never-refilled page");
}

#[test]
fn control_reseed32_caught_only_by_collisions() {
    // A perfect CSPRNG with a 32-bit state: passes the distribution
    // battery, caught by the birthday test. This is the whole reason
    // collision_freedom exists.
    let mut s = controls::Reseed32::new(1);
    let dist = battery::battery_from(32, |o| s.fill(o));
    println!("reseed32 distribution report:\n{}", dist.summary());

    let mut s2 = controls::Reseed32::new(7);
    let t = std::time::Instant::now();
    let coll = battery::collision_freedom(|o| s2.draw32(o));
    println!("reseed32 collision test took {:?}\n{}", t.elapsed(), coll.summary());
    assert!(!coll.passed(), "32-bit-state generator MUST collide within {} draws", battery::COLLISION_DRAWS);
}

#[test]
fn control_fixed_seed_passes_and_that_is_the_point() {
    // disclosure bug 1: statistically perfect, undetectable here. The
    // detectors are the backend/graph contract tests in rng_backend.rs
    // and cross-boot independence on hardware.
    let mut s = controls::FixedSeed::new(0x42);
    let r = battery::battery_from(32, |o| s.fill(o));
    assert!(
        r.passed(),
        "a fixed-seed CSPRNG is expected to PASS the statistics; if it now \
         fails, the battery changed meaning:\n{}",
        r.summary()
    );
    let mut a = controls::FixedSeed::new(0x42);
    let mut b = controls::FixedSeed::new(0x42);
    let (mut x, mut y) = ([0u8; 32], [0u8; 32]);
    a.fill(&mut x);
    b.fill(&mut y);
    assert_eq!(x, y, "two instances of a fixed-seed PRNG must agree — that IS the bug shape");
}

// =====================================================================
// Task 2 — key-validity / bit-length contract
//
// `scalar_from_bytes` is `pub(crate)` in `wallet-core/src/keys.rs`, not
// reachable from this external integration-test crate, and per the brief
// we must not widen its visibility just for a test. `compressed_pubkey`
// and `xonly_pubkey` are its public callers: both return
// `Err(Error::InvalidPrivateKey)` iff `scalar_from_bytes` rejects the key
// (zero or >= the curve order N), so a successful call is the public
// equivalent of the private scalar check.
// =====================================================================

/// How many keys to draw for the validity/round-trip contract. Cheap
/// enough (EC point multiplication + WIF codec) to run generously without
/// slowing the suite down like the statistical battery does.
const VALIDITY_DRAWS: usize = 2_000;

#[test]
fn generated_keys_are_32_bytes() {
    for _ in 0..VALIDITY_DRAWS {
        let k = keys::generate_private_key().unwrap();
        // The return type is `[u8; 32]` — this is a tautology at the type
        // level, but pin it as an explicit, load-bearing assertion so a
        // future signature change (e.g. a Vec<u8> refactor) trips a test
        // instead of silently changing the key's bit length.
        assert_eq!(k.len(), 32, "gift key must be exactly 256 bits");
    }
}

#[test]
fn generated_keys_are_valid_secp256k1_scalars() {
    for _ in 0..VALIDITY_DRAWS {
        let k = keys::generate_private_key().unwrap();
        // 0 < k < N, exercised via the public surface (see module doc).
        keys::compressed_pubkey(&k).expect("generated key must be a valid scalar (compressed_pubkey)");
        keys::xonly_pubkey(&k).expect("generated key must be a valid scalar (xonly_pubkey)");
    }
}

#[test]
fn generated_keys_round_trip_wif_and_pubkey() {
    for _ in 0..VALIDITY_DRAWS {
        let k = keys::generate_private_key().unwrap();

        // WIF round-trip: a truncation/off-by-one anywhere in the base58
        // check-encode path would show up as a decode failure or a key
        // mismatch here.
        let wif = keys::wif_encode(&k);
        let decoded = keys::wif_decode(&wif).expect("freshly generated key must produce a decodable WIF");
        assert_eq!(decoded, k, "WIF round-trip must reproduce the exact 32 bytes");

        // Compressed pubkey: fixed 33-byte SEC1 point with a valid prefix.
        let pubkey = keys::compressed_pubkey(&k).unwrap();
        assert_eq!(pubkey.len(), 33);
        assert!(pubkey[0] == 0x02 || pubkey[0] == 0x03, "compressed pubkey prefix must be 0x02/0x03");

        // Full pipeline: build an actual Segwit wallet from the key and
        // check the address/WIF the bill would print — so a truncation
        // bug anywhere between keygen and the printed bill shows up here,
        // not just in the codec functions in isolation.
        let wallet = wallet_core::from_privkeys(wallet_core::Variant::Segwit, &k, None).unwrap();
        assert_eq!(wallet.private_key_wif, wif);
        assert!(wallet.address.starts_with("bc1q"), "segwit address prefix");
    }
}

/// Gap noted per the brief: `generate_private_key`'s bounded 128-draw
/// rejection loop (`wallet-core/src/keys.rs`) is NOT injectable — it calls
/// `getrandom::getrandom` directly with no seam for a simulated broken
/// source, and the brief is explicit that production code must not be
/// refactored to add one just for testability. A real out-of-range scalar
/// draw has probability ~2^-128 (N is that close to 2^256), so the retry
/// path is not reachable by drawing real entropy either — not even at
/// `VALIDITY_DRAWS` or `COLLISION_DRAWS` volumes.
///
/// What IS observable and is asserted above: every draw the function
/// actually returns is a valid, in-range scalar (`generated_keys_are_
/// valid_secp256k1_scalars`), which is the loop's postcondition. The
/// loop's *behavior when the source is broken* (bounded retries, then
/// `Error::Entropy` rather than spinning or returning garbage) stays
/// unverified by this suite; the sentinel-based rejection in
/// `vendor/getrandom/src/xous.rs` (see `trng_check.rs`) is the mechanism
/// that would actually make the source "broken" on device, and that path
/// IS exercised by `wallet-core/tests/trng_check.rs`.
#[test]
fn gap_note_bounded_retry_not_independently_injectable() {
    // Intentionally not a real assertion beyond "doesn't panic" — see the
    // doc comment above. Kept as a named, discoverable test rather than a
    // comment so the gap shows up in `cargo test` output, not just in
    // prose that can go stale.
    let k = keys::generate_private_key().unwrap();
    assert_eq!(k.len(), 32);
}
