//! Bill PNG composition — box-for-box port of the web app's
//! `js/bill_generator.js` (1843×784, overlays onto assets/bill_template.png).
//! Time is injected by the caller so composition is deterministic for tests.

use image::codecs::png::{CompressionType, FilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, Rgb, RgbImage};

use crate::qr::{self, QrMatrix};
use crate::text::{self, BillFont};
use crate::{template, Error, Wallet};

pub const BILL_WIDTH: u32 = 1843;
pub const BILL_HEIGHT: u32 = 784;

/// The default design's artwork. `pub(crate)` so `template::BUILTIN_DESIGNS`
/// can point at the same bytes rather than `include_bytes!`-ing a second
/// copy of a half-megabyte asset into the binary.
pub(crate) static TEMPLATE: &[u8] = include_bytes!("../assets/bill_template.png");

const BANNER_COLOR: Rgb<u8> = Rgb([253, 229, 167]);
// Motto/year ink. Was the old artwork's cyan-blue; the replacement bill is
// engraved in teal and orange, and blue read as a leftover from another
// design. Changing it moves composed output, so the masters move with it.
const BANNER_TEXT_COLOR: Rgb<u8> = Rgb([16, 58, 74]);
pub(crate) const INK: Rgb<u8> = Rgb([30, 30, 30]);
pub(crate) const BLACK: Rgb<u8> = Rgb([0, 0, 0]);
pub(crate) const WHITE: Rgb<u8> = Rgb([255, 255, 255]);

// The satoshi-only banner boxes; the content regions live in
// `template::satoshi_spec` (the classic bill_generator.js layout).
const BANNER_LEFT_BOX: (u32, u32, u32, u32) = (1082, 305, 1225, 339);
const BANNER_RIGHT_BOX: (u32, u32, u32, u32) = (1326, 305, 1425, 339);

pub(crate) fn fill_rect(img: &mut RgbImage, x1: u32, y1: u32, x2: u32, y2: u32, color: Rgb<u8>) {
    for y in y1..y2.min(img.height()) {
        for x in x1..x2.min(img.width()) {
            img.put_pixel(x, y, color);
        }
    }
}

/// `qrToCanvas` twin: white background of round((size+2*border)*module_size)
/// square at (x, y), dark modules with integer-rounded shared edges.
fn draw_qr(img: &mut RgbImage, matrix: &QrMatrix, x: f32, y: f32, module_size: f32, border: f32) {
    let size = matrix.width;
    let total = ((size as f32 + 2.0 * border) * module_size).round() as u32;
    fill_rect(
        img,
        x as u32,
        y as u32,
        x as u32 + total,
        y as u32 + total,
        WHITE,
    );
    for r in 0..size {
        for c in 0..size {
            if matrix.modules[r * size + c] {
                let px = (x + (c as f32 + border) * module_size).round() as i64;
                let py = (y + (r as f32 + border) * module_size).round() as i64;
                let px2 = (x + (c as f32 + border + 1.0) * module_size).round() as i64;
                let py2 = (y + (r as f32 + border + 1.0) * module_size).round() as i64;
                fill_rect(img, px as u32, py as u32, px2 as u32, py2 as u32, BLACK);
            }
        }
    }
}

pub(crate) fn paste_qr_in_box(
    img: &mut RgbImage,
    payload: &str,
    bx: (f32, f32, f32, f32),
) -> Result<(), Error> {
    let (x1, y1, x2, y2) = bx;
    let box_w = x2 - x1;
    let box_h = y2 - y1;
    let qr_side = box_w.min(box_h);
    let matrix = qr::qr_matrix(payload)?;
    let paste_x = x1 + ((box_w - qr_side) / 2.0).floor();
    let paste_y = y1 + ((box_h - qr_side) / 2.0).floor();
    let module_size = qr_side / (matrix.width as f32 + 4.0);
    draw_qr(img, &matrix, paste_x, paste_y, module_size, 2.0);
    Ok(())
}

/// The embedded satoshi artwork (no markers), decoded.
pub(crate) fn decode_satoshi_art() -> Result<RgbImage, Error> {
    let img = image::load_from_memory_with_format(TEMPLATE, image::ImageFormat::Png)
        .map_err(|_| Error::Render)?
        .to_rgb8();
    if img.dimensions() != (BILL_WIDTH, BILL_HEIGHT) {
        return Err(Error::Render);
    }
    Ok(img)
}

/// Encode with the settings every bill/kit PNG uses.
pub(crate) fn encode_png(img: &RgbImage) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(img, img.width(), img.height(), ExtendedColorType::Rgb8)
        .map_err(|_| Error::Render)?;
    Ok(out)
}

/// Compose the built-in satoshi bill for `wallet`; `year` and `timestamp_utc`
/// (e.g. "2026" / "2026-07-02 21:14:03 UTC") are injected for determinism.
/// Runs the satoshi-only banner pass, then the same marker engine custom
/// templates use (`template::compose_on`, spec from `template::satoshi_spec`
/// — the unmarked art has no marker pixels, so healing is a no-op). Output
/// is pinned byte-for-byte by tests/fixtures/master_satoshi_*.png.
pub fn compose_bill(wallet: &Wallet, year: &str, timestamp_utc: &str) -> Result<Vec<u8>, Error> {
    let mut img = decode_satoshi_art()?;

    // 0. Banner: cover old text, redraw motto and year (web draws these in a
    // bold sans; DejaVu Sans Condensed regular is our embeddable stand-in).
    fill_rect(
        &mut img,
        BANNER_LEFT_BOX.0,
        BANNER_LEFT_BOX.1,
        BANNER_LEFT_BOX.2,
        BANNER_LEFT_BOX.3,
        BANNER_COLOR,
    );
    fill_rect(
        &mut img,
        BANNER_RIGHT_BOX.0,
        BANNER_RIGHT_BOX.1,
        BANNER_RIGHT_BOX.2,
        BANNER_RIGHT_BOX.3,
        BANNER_COLOR,
    );
    {
        let (lx1, ly1) = (1082.0f32 + 4.0, 301.0f32);
        let (lw, lh) = (1225.0 - 2.0 - lx1, 343.0 - ly1);
        let motto = "VIRES IN NUMERIS";
        let (size, width) = text::fit_to_box(BillFont::Condensed, motto, lw, lh, 24);
        let tx = lx1 + (lw - width) / 2.0;
        let ty = ly1 + (lh + size * 0.8) / 2.0;
        text::draw_text(&mut img, BillFont::Condensed, size, tx, ty, BANNER_TEXT_COLOR, motto);
    }
    {
        let (rx1, ry1) = (BANNER_RIGHT_BOX.0 as f32, BANNER_RIGHT_BOX.1 as f32);
        let rw = (BANNER_RIGHT_BOX.2 - BANNER_RIGHT_BOX.0) as f32;
        let rh = (BANNER_RIGHT_BOX.3 - BANNER_RIGHT_BOX.1) as f32;
        let (size, width) = text::fit_to_box(BillFont::Condensed, year, rw, rh, 24);
        let tx = rx1 + (rw - width) / 2.0;
        let ty = ry1 + (rh + size * 0.8) / 2.0;
        text::draw_text(&mut img, BillFont::Condensed, size, tx, ty, BANNER_TEXT_COLOR, year);
    }

    // 1-5. QRs, "(tweaked)" label, WIF strip, address band, timestamp —
    // the shared marker engine, at the classic satoshi positions.
    template::compose_on(&mut img, &template::satoshi_spec(), wallet, timestamp_utc)?;

    encode_png(&img)
}

/// Format a unix timestamp as the bill's ("YYYY", "YYYY-MM-DD HH:MM:SS UTC")
/// pair. Civil-from-days per Howard Hinnant's algorithm; no clock access here
/// — the app injects the seconds so composition stays deterministic.
pub fn format_utc(unix_secs: i64) -> (String, String) {
    let days = unix_secs.div_euclid(86_400);
    let secs_of_day = unix_secs.rem_euclid(86_400);
    let (h, mi, s) = (secs_of_day / 3600, (secs_of_day % 3600) / 60, secs_of_day % 60);

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    (
        format!("{y}"),
        format!("{y}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC"),
    )
}
