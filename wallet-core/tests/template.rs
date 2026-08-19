//! Marker-template engine tests.
//!
//! The headline contract: `compose_bill` (built-in satoshi, now routed
//! through the shared engine) must equal the committed masters in
//! tests/fixtures/ BYTE-FOR-BYTE, and detection on the marked satoshi
//! example must recover exactly the spec the built-in uses. Together these
//! pin both halves of what a custom template exercises.

use image::{Rgb, RgbImage};
use wallet_core::template::{
    self, Rect, Region, TemplateError, TemplateSpec, MAX_TEMPLATE_SIDE,
};
use wallet_core::{bill, qr, Variant};

const K1: [u8; 32] = {
    let mut k = [0u8; 32];
    k[31] = 1;
    k
};
const KBACKUP: [u8; 32] = [0x11; 32];
const TS: &str = "2026-07-02 12:00:00 UTC";
const YEAR: &str = "2026";

fn master(variant: Variant) -> &'static [u8] {
    match variant {
        Variant::Segwit => include_bytes!("fixtures/master_satoshi_segwit.png"),
        Variant::Taproot => include_bytes!("fixtures/master_satoshi_taproot.png"),
        Variant::TaprootBackup => include_bytes!("fixtures/master_satoshi_taproot_backup.png"),
    }
}

fn wallet_for(variant: Variant) -> wallet_core::Wallet {
    let backup = matches!(variant, Variant::TaprootBackup).then_some((KBACKUP, 0));
    wallet_core::from_privkeys(variant, &K1, backup).unwrap()
}

fn decode_qrs(png: &[u8]) -> Vec<String> {
    let img = image::load_from_memory(png).unwrap().to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(img);
    prepared
        .detect_grids()
        .into_iter()
        .filter_map(|g| g.decode().ok().map(|(_, content)| content))
        .collect()
}

fn encode(img: &RgbImage) -> Vec<u8> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(img, img.width(), img.height(), ExtendedColorType::Rgb8)
        .unwrap();
    out
}

fn fill(img: &mut RgbImage, r: Rect, color: [u8; 3]) {
    for y in r.y1..r.y2 {
        for x in r.x1..r.x2 {
            img.put_pixel(x, y, Rgb(color));
        }
    }
}

/// A synthetic 1000×500 template with all five regions on a beige ground.
fn synthetic_full() -> (RgbImage, TemplateSpec) {
    let mut img = RgbImage::from_pixel(1000, 500, Rgb([235, 224, 200]));
    let spec = TemplateSpec {
        width: 1000,
        height: 500,
        address_qr: Rect { x1: 30, y1: 200, x2: 280, y2: 450 },
        privkey_qr: Rect { x1: 720, y1: 30, x2: 970, y2: 280 },
        address_text: Some(Rect { x1: 300, y1: 420, x2: 700, y2: 470 }),
        privkey_text: Some(Rect { x1: 30, y1: 20, x2: 700, y2: 48 }),
        timestamp: Some(Rect { x1: 310, y1: 300, x2: 690, y2: 320 }),
    };
    fill(&mut img, spec.address_qr, Region::AddressQr.marker_rgb());
    fill(&mut img, spec.privkey_qr, Region::PrivkeyQr.marker_rgb());
    fill(&mut img, spec.address_text.unwrap(), Region::AddressText.marker_rgb());
    fill(&mut img, spec.privkey_text.unwrap(), Region::PrivkeyText.marker_rgb());
    fill(&mut img, spec.timestamp.unwrap(), Region::Timestamp.marker_rgb());
    (img, spec)
}

fn assert_no_marker_pixels(png: &[u8]) {
    let img = image::load_from_memory(png).unwrap().to_rgb8();
    for region in template::ALL_REGIONS {
        let marker = Rgb(region.marker_rgb());
        assert!(
            !img.pixels().any(|p| *p == marker),
            "{} marker color survived composition",
            region.display_name()
        );
    }
}

// ---- master parity -------------------------------------------------------

#[test]
fn builtin_satoshi_matches_masters_byte_for_byte() {
    for variant in [Variant::Segwit, Variant::Taproot, Variant::TaprootBackup] {
        let png = bill::compose_bill(&wallet_for(variant), YEAR, TS).unwrap();
        assert_eq!(
            png,
            master(variant),
            "{variant:?}: engine output diverged from the committed master"
        );
    }
}

#[test]
fn marked_satoshi_example_detects_the_builtin_spec() {
    let kit = template::render_design_kit().unwrap();
    let spec = template::validate_template(&kit.satoshi_example_png).unwrap();
    assert_eq!(spec, template::satoshi_spec());
}

/// The marked satoshi example through the full custom pipeline must equal
/// the master on every pixel outside the marker rects and banner boxes
/// (inside them, healing flat-fills what the markers destroyed and the
/// custom path draws no banner year).
#[test]
fn marked_satoshi_example_matches_master_outside_markers() {
    let kit = template::render_design_kit().unwrap();
    let spec = template::satoshi_spec();
    for variant in [Variant::Segwit, Variant::TaprootBackup] {
        let wallet = wallet_for(variant);
        let custom = template::compose_custom_bill(&kit.satoshi_example_png, &wallet, TS).unwrap();
        let custom = image::load_from_memory(&custom).unwrap().to_rgb8();
        let master = image::load_from_memory(master(variant)).unwrap().to_rgb8();

        let masked: &[Rect] = &[
            spec.address_qr,
            spec.privkey_qr,
            spec.address_text.unwrap(),
            spec.privkey_text.unwrap(),
            spec.timestamp.unwrap(),
            // Banner boxes incl. the motto/year text overshoot (bill.rs).
            Rect { x1: 1082, y1: 301, x2: 1425, y2: 343 },
        ];
        let mut diff = 0u32;
        for (x, y, px) in custom.enumerate_pixels() {
            if masked.iter().any(|r| x >= r.x1 && x < r.x2 && y >= r.y1 && y < r.y2) {
                continue;
            }
            if master.get_pixel(x, y) != px {
                diff += 1;
            }
        }
        assert_eq!(diff, 0, "{variant:?}: unmasked pixels diverged from master");

        let contents = decode_qrs(&encode(&custom));
        assert!(contents.contains(&qr::address_payload(&wallet.address)));
        assert!(contents.contains(&qr::sweep_url(&wallet.bill_wif, variant)));
    }
}

// ---- detection -----------------------------------------------------------

#[test]
fn detect_recovers_exact_rects() {
    let (img, expected) = synthetic_full();
    let spec = TemplateSpec::detect(&img).unwrap();
    assert_eq!(spec, expected);
}

#[test]
fn optional_markers_absent_are_skipped_and_compose_succeeds() {
    let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
    let aqr = Rect { x1: 20, y1: 20, x2: 220, y2: 220 };
    let pqr = Rect { x1: 350, y1: 150, x2: 550, y2: 350 };
    fill(&mut img, aqr, Region::AddressQr.marker_rgb());
    fill(&mut img, pqr, Region::PrivkeyQr.marker_rgb());
    let spec = TemplateSpec::detect(&img).unwrap();
    assert_eq!(spec.address_text, None);
    assert_eq!(spec.privkey_text, None);
    assert_eq!(spec.timestamp, None);

    let wallet = wallet_for(Variant::Segwit);
    let png = template::compose_custom_bill(&encode(&img), &wallet, TS).unwrap();
    let contents = decode_qrs(&png);
    assert_eq!(contents.len(), 2);
    assert_no_marker_pixels(&png);
}

#[test]
fn missing_required_marker_errors() {
    let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
    fill(&mut img, Rect { x1: 20, y1: 20, x2: 220, y2: 220 }, Region::AddressQr.marker_rgb());
    assert_eq!(
        TemplateSpec::detect(&img),
        Err(TemplateError::MissingMarker(Region::PrivkeyQr))
    );
}

#[test]
fn two_disjoint_rects_of_one_color_fail_solidity() {
    let (mut img, _) = synthetic_full();
    // Second magenta rect far from the real one → merged bbox is hollow.
    fill(&mut img, Rect { x1: 900, y1: 400, x2: 990, y2: 490 }, Region::AddressQr.marker_rgb());
    assert_eq!(
        TemplateSpec::detect(&img),
        Err(TemplateError::BadMarker(Region::AddressQr))
    );
}

#[test]
fn stray_pixels_do_not_become_a_region() {
    let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
    fill(&mut img, Rect { x1: 20, y1: 20, x2: 220, y2: 220 }, Region::AddressQr.marker_rgb());
    fill(&mut img, Rect { x1: 350, y1: 150, x2: 550, y2: 350 }, Region::PrivkeyQr.marker_rgb());
    // A 10×10 green blob: under both the 256-px and 16-px-side floors.
    fill(&mut img, Rect { x1: 300, y1: 30, x2: 310, y2: 40 }, Region::AddressText.marker_rgb());
    let spec = TemplateSpec::detect(&img).unwrap();
    assert_eq!(spec.address_text, None);
}

#[test]
fn anti_aliased_edge_still_detects_solid_core() {
    let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
    let core = Rect { x1: 50, y1: 50, x2: 250, y2: 250 };
    fill(&mut img, core, Region::AddressQr.marker_rgb());
    fill(&mut img, Rect { x1: 350, y1: 150, x2: 550, y2: 350 }, Region::PrivkeyQr.marker_rgb());
    // Blended (non-exact) ring like an editor's anti-aliasing would leave.
    for x in 49..251 {
        img.put_pixel(x, 49, Rgb([255, 128, 255]));
        img.put_pixel(x, 250, Rgb([255, 128, 255]));
    }
    let spec = TemplateSpec::detect(&img).unwrap();
    assert_eq!(spec.address_qr, core);
}

#[test]
fn too_small_qr_marker_errors() {
    let mut img = RgbImage::from_pixel(600, 400, Rgb([255, 255, 255]));
    fill(&mut img, Rect { x1: 20, y1: 20, x2: 60, y2: 60 }, Region::AddressQr.marker_rgb());
    fill(&mut img, Rect { x1: 350, y1: 150, x2: 550, y2: 350 }, Region::PrivkeyQr.marker_rgb());
    assert_eq!(
        TemplateSpec::detect(&img),
        Err(TemplateError::BadMarker(Region::AddressQr))
    );
}

#[test]
fn oversized_canvas_and_garbage_bytes_error() {
    let img = RgbImage::from_pixel(MAX_TEMPLATE_SIDE + 1, 10, Rgb([255, 255, 255]));
    assert_eq!(
        template::validate_template(&encode(&img)),
        Err(TemplateError::TooLarge { w: MAX_TEMPLATE_SIDE + 1, h: 10 })
    );
    assert_eq!(template::validate_template(b"not a png"), Err(TemplateError::Png));
}

// ---- healing + composition on custom layouts ------------------------------

#[test]
fn text_marker_heals_to_surrounding_color() {
    let ground = [235, 224, 200];
    let (img, spec) = synthetic_full();
    let wallet = wallet_for(Variant::Segwit);
    let png = template::compose_custom_bill(&encode(&img), &wallet, TS).unwrap();
    assert_no_marker_pixels(&png);
    let out = image::load_from_memory(&png).unwrap().to_rgb8();
    // Corner of the timestamp strip: healed to the ground color (the strip
    // is wider than the drawn text, so its corners hold pure heal pixels).
    let ts = spec.timestamp.unwrap();
    assert_eq!(out.get_pixel(ts.x1, ts.y1).0, ground);
}

#[test]
fn custom_template_round_trips_qrs_all_variants() {
    let (img, _) = synthetic_full();
    let png_template = encode(&img);
    for variant in [Variant::Segwit, Variant::Taproot, Variant::TaprootBackup] {
        let wallet = wallet_for(variant);
        let png = template::compose_custom_bill(&png_template, &wallet, TS).unwrap();
        let contents = decode_qrs(&png);
        assert_eq!(contents.len(), 2, "{variant:?}: expected both QRs to scan");
        assert!(contents.contains(&qr::address_payload(&wallet.address)));
        assert!(contents.contains(&qr::sweep_url(&wallet.bill_wif, variant)));
        assert_no_marker_pixels(&png);
    }
}

/// Nothing in the engine may assume the classic landscape shape: a square
/// canvas with side-by-side QRs, a text plate, and a horizontal timestamp.
#[test]
fn square_canvas_composes_and_round_trips() {
    let mut img = RgbImage::from_pixel(1000, 1000, Rgb([247, 243, 233]));
    let spec = TemplateSpec {
        width: 1000,
        height: 1000,
        address_qr: Rect { x1: 60, y1: 200, x2: 460, y2: 600 },
        privkey_qr: Rect { x1: 540, y1: 200, x2: 940, y2: 600 },
        address_text: Some(Rect { x1: 100, y1: 660, x2: 900, y2: 730 }),
        privkey_text: Some(Rect { x1: 60, y1: 52, x2: 940, y2: 94 }),
        timestamp: Some(Rect { x1: 100, y1: 780, x2: 900, y2: 810 }),
    };
    // Dark plate behind the WIF marker: exercises the adaptive (light) ink.
    fill(&mut img, Rect { x1: 40, y1: 40, x2: 960, y2: 106 }, [28, 50, 46]);
    fill(&mut img, spec.address_qr, Region::AddressQr.marker_rgb());
    fill(&mut img, spec.privkey_qr, Region::PrivkeyQr.marker_rgb());
    fill(&mut img, spec.address_text.unwrap(), Region::AddressText.marker_rgb());
    fill(&mut img, spec.privkey_text.unwrap(), Region::PrivkeyText.marker_rgb());
    fill(&mut img, spec.timestamp.unwrap(), Region::Timestamp.marker_rgb());

    let template_png = encode(&img);
    assert_eq!(TemplateSpec::detect(&img).unwrap(), spec);
    let wallet = wallet_for(Variant::TaprootBackup);
    let png = template::compose_custom_bill(&template_png, &wallet, TS).unwrap();
    assert_no_marker_pixels(&png);
    let out = image::load_from_memory(&png).unwrap().to_rgb8();
    assert_eq!(out.dimensions(), (1000, 1000));
    // WIF marker healed back to the dark plate color at an untouched corner.
    assert_eq!(out.get_pixel(60, 52).0, [28, 50, 46]);
    let contents = decode_qrs(&png);
    assert_eq!(contents.len(), 2, "square canvas: expected both QRs to scan");
    assert!(contents.contains(&qr::address_payload(&wallet.address)));
    assert!(contents.contains(&qr::sweep_url(&wallet.bill_wif, Variant::TaprootBackup)));
}

// ---- design kit -----------------------------------------------------------

#[test]
fn design_kit_files_are_valid_templates() {
    let kit = template::render_design_kit().unwrap();
    let blank_spec = template::validate_template(&kit.template_png).unwrap();
    assert_eq!(blank_spec, template::satoshi_spec());
    template::validate_template(&kit.satoshi_example_png).unwrap();
    assert!(template::design_kit_readme().contains("magenta"));
}

/// The two extra designs shipped in `assets/templates/` are marker templates,
/// so they take exactly the path a user's own PNG takes. Both halves are
/// asserted because a template can fail in two very different ways: markers
/// that no longer detect (a quantizer shifting one channel is enough), and
/// markers that detect but land somewhere the content does not fit.
///
/// They are generated by `assets/art/generate_bill_art.py`; if that script
/// changes, this is what catches a design that stopped being usable.
#[test]
fn shipped_marker_templates_detect_and_compose() {
    let designs: [(&str, &[u8]); 2] = [
        ("cypherpunk", include_bytes!("../assets/templates/cypherpunk.png")),
        ("ledger", include_bytes!("../assets/templates/ledger.png")),
    ];
    for (name, png) in designs {
        let img = image::load_from_memory(png)
            .unwrap_or_else(|e| panic!("{name}: decode: {e}"))
            .to_rgb8();
        let spec = TemplateSpec::detect(&img).unwrap_or_else(|e| panic!("{name}: detect: {e}"));
        assert_eq!(
            spec,
            template::satoshi_spec(),
            "{name} detects a layout other than the built-in one"
        );

        for variant in [Variant::Segwit, Variant::Taproot, Variant::TaprootBackup] {
            let wallet = wallet_for(variant);
            let out = template::compose_custom_bill(png, &wallet, TS)
                .unwrap_or_else(|e| panic!("{name}/{variant:?}: compose: {e}"));
            let payloads = decode_qrs(&out);
            assert!(
                payloads.contains(&qr::address_payload(&wallet.address)),
                "{name}/{variant:?}: address QR did not decode back to the address"
            );
            assert!(
                payloads.contains(&qr::sweep_url(&wallet.bill_wif, wallet.variant)),
                "{name}/{variant:?}: private-key QR did not decode back to the sweep URL"
            );
        }
    }
}
