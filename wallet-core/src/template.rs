//! Marker-driven bill templates: a template is a plain PNG in which solid
//! rectangles of reserved colors mark the fillable regions. Detection finds
//! each region, composition heals the marker pixels and draws the content.
//!
//! The built-in satoshi bill runs through this same engine (spec from
//! [`satoshi_spec`], banner pass layered on top in `bill.rs`); the committed
//! masters in `tests/fixtures/` pin its output byte-for-byte, which is the
//! correctness contract for everything custom templates use.

use std::collections::HashMap;

use image::{Rgb, RgbImage};

use crate::bill::{self, BLACK, INK, WHITE};
use crate::text::{self, BillFont};
use crate::{qr, Error, Variant, Wallet};

/// Sanity cap per side; bounds decode memory on device.
pub const MAX_TEMPLATE_SIDE: u32 = 4096;

/// Smallest exact-match pixel count / bbox side for a marker to count as a
/// region at all (below this it is treated as stray artwork pixels).
const MIN_MARKER_PIXELS: u32 = 256;
const MIN_MARKER_SIDE: u32 = 16;
/// A marker must be one solid rectangle: exact-match pixels must cover at
/// least this fraction of the bounding box (tolerates anti-aliased edges).
const MIN_SOLIDITY: f32 = 0.90;
/// QR regions need room for a scannable code.
const MIN_QR_SIDE: u32 = 64;

/// Maximum font sizes per text region — identical to the satoshi bill's
/// hardcoded fit starts, so the marked-satoshi template reproduces the master.
const ADDRESS_TEXT_MAX_PX: u32 = 36;
const PRIVKEY_TEXT_MAX_PX: u32 = 24;
const TIMESTAMP_MAX_PX: u32 = 14;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    AddressQr,
    PrivkeyQr,
    AddressText,
    PrivkeyText,
    Timestamp,
}

pub const ALL_REGIONS: [Region; 5] = [
    Region::AddressQr,
    Region::PrivkeyQr,
    Region::AddressText,
    Region::PrivkeyText,
    Region::Timestamp,
];

impl Region {
    pub const fn marker_rgb(self) -> [u8; 3] {
        match self {
            Region::AddressQr => [255, 0, 255],
            Region::PrivkeyQr => [0, 255, 255],
            Region::AddressText => [0, 255, 0],
            Region::PrivkeyText => [255, 0, 0],
            Region::Timestamp => [0, 0, 255],
        }
    }

    pub const fn color_name(self) -> &'static str {
        match self {
            Region::AddressQr => "magenta",
            Region::PrivkeyQr => "cyan",
            Region::AddressText => "green",
            Region::PrivkeyText => "red",
            Region::Timestamp => "blue",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Region::AddressQr => "address QR",
            Region::PrivkeyQr => "private key QR",
            Region::AddressText => "address text",
            Region::PrivkeyText => "private key text",
            Region::Timestamp => "timestamp",
        }
    }

    pub const fn required(self) -> bool {
        matches!(self, Region::AddressQr | Region::PrivkeyQr)
    }

    const fn is_qr(self) -> bool {
        matches!(self, Region::AddressQr | Region::PrivkeyQr)
    }
}

/// Pixel rectangle; `x2`/`y2` exclusive (the `fill_rect` convention).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x1: u32,
    pub y1: u32,
    pub x2: u32,
    pub y2: u32,
}

impl Rect {
    fn as_box(self) -> (f32, f32, f32, f32) {
        (self.x1 as f32, self.y1 as f32, self.x2 as f32, self.y2 as f32)
    }

    fn width(self) -> u32 {
        self.x2 - self.x1
    }

    fn height(self) -> u32 {
        self.y2 - self.y1
    }
}

/// The fillable regions found in a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateSpec {
    pub width: u32,
    pub height: u32,
    pub address_qr: Rect,
    pub privkey_qr: Rect,
    pub address_text: Option<Rect>,
    pub privkey_text: Option<Rect>,
    pub timestamp: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateError {
    /// Not decodable as a PNG image.
    Png,
    TooLarge { w: u32, h: u32 },
    MissingMarker(Region),
    /// Marker pixels exist but do not form one usable solid rectangle.
    BadMarker(Region),
    Compose(Error),
}

impl From<Error> for TemplateError {
    fn from(e: Error) -> Self {
        TemplateError::Compose(e)
    }
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::Png => f.write_str("Template is not a readable PNG"),
            TemplateError::TooLarge { w, h } => write!(
                f,
                "Template is too large ({w}\u{d7}{h}; max {MAX_TEMPLATE_SIDE}\u{d7}{MAX_TEMPLATE_SIDE} pixels)"
            ),
            TemplateError::MissingMarker(r) => write!(
                f,
                "Template is missing the {} marker ({} rectangle)",
                r.display_name(),
                r.color_name()
            ),
            TemplateError::BadMarker(r) => {
                write!(
                    f,
                    "The {} marker ({}) must be one solid rectangle",
                    r.display_name(),
                    r.color_name()
                )?;
                if r.is_qr() {
                    write!(f, ", at least {MIN_QR_SIDE} pixels per side")?;
                }
                Ok(())
            }
            TemplateError::Compose(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for TemplateError {}

impl TemplateSpec {
    /// Scan `img` for the reserved marker colors. One pass; per color the
    /// exact-match bounding box + count, then the stray/solidity/size rules.
    pub fn detect(img: &RgbImage) -> Result<TemplateSpec, TemplateError> {
        struct Acc {
            count: u32,
            x1: u32,
            y1: u32,
            x2: u32,
            y2: u32,
        }
        let mut accs: [Option<Acc>; 5] = [None, None, None, None, None];
        for (x, y, px) in img.enumerate_pixels() {
            let idx = match px.0 {
                [255, 0, 255] => 0,
                [0, 255, 255] => 1,
                [0, 255, 0] => 2,
                [255, 0, 0] => 3,
                [0, 0, 255] => 4,
                _ => continue,
            };
            match &mut accs[idx] {
                Some(a) => {
                    a.count += 1;
                    a.x1 = a.x1.min(x);
                    a.y1 = a.y1.min(y);
                    a.x2 = a.x2.max(x);
                    a.y2 = a.y2.max(y);
                }
                slot => *slot = Some(Acc { count: 1, x1: x, y1: y, x2: x, y2: y }),
            }
        }

        let mut rects: [Option<Rect>; 5] = [None; 5];
        for (i, region) in ALL_REGIONS.iter().enumerate() {
            let acc = match &accs[i] {
                Some(a) => a,
                None => continue,
            };
            let rect = Rect { x1: acc.x1, y1: acc.y1, x2: acc.x2 + 1, y2: acc.y2 + 1 };
            let min_side = rect.width().min(rect.height());
            if acc.count < MIN_MARKER_PIXELS || min_side < MIN_MARKER_SIDE {
                continue; // stray artwork pixels, not a marker
            }
            let area = (rect.width() * rect.height()) as f32;
            if (acc.count as f32) < MIN_SOLIDITY * area {
                return Err(TemplateError::BadMarker(*region));
            }
            if region.is_qr() && min_side < MIN_QR_SIDE {
                return Err(TemplateError::BadMarker(*region));
            }
            rects[i] = Some(rect);
        }

        Ok(TemplateSpec {
            width: img.width(),
            height: img.height(),
            address_qr: rects[0].ok_or(TemplateError::MissingMarker(Region::AddressQr))?,
            privkey_qr: rects[1].ok_or(TemplateError::MissingMarker(Region::PrivkeyQr))?,
            address_text: rects[2],
            privkey_text: rects[3],
            timestamp: rects[4],
        })
    }

    fn region_rect(&self, region: Region) -> Option<Rect> {
        match region {
            Region::AddressQr => Some(self.address_qr),
            Region::PrivkeyQr => Some(self.privkey_qr),
            Region::AddressText => self.address_text,
            Region::PrivkeyText => self.privkey_text,
            Region::Timestamp => self.timestamp,
        }
    }
}

/// Decode + size-check + detect: everything the app needs to accept or
/// reject a picked template file.
pub fn validate_template(png: &[u8]) -> Result<TemplateSpec, TemplateError> {
    let img = decode_template(png)?;
    TemplateSpec::detect(&img)
}

fn decode_template(png: &[u8]) -> Result<RgbImage, TemplateError> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .map_err(|_| TemplateError::Png)?;
    let (w, h) = (img.width(), img.height());
    if w > MAX_TEMPLATE_SIDE || h > MAX_TEMPLATE_SIDE {
        return Err(TemplateError::TooLarge { w, h });
    }
    Ok(img.to_rgb8())
}

/// Compose a bill from a custom template PNG (detect + heal + draw + encode).
/// No banner/year — that decoration is satoshi-only.
pub fn compose_custom_bill(
    template_png: &[u8],
    wallet: &Wallet,
    timestamp_utc: &str,
) -> Result<Vec<u8>, TemplateError> {
    let mut img = decode_template(template_png)?;
    let spec = TemplateSpec::detect(&img)?;
    compose_on(&mut img, &spec, wallet, timestamp_utc)?;
    Ok(bill::encode_png(&img)?)
}

/// The engine: heal every region's marker pixels, then draw the content.
/// Also the built-in satoshi path (`bill::compose_bill`) — there the image
/// carries no marker pixels, so healing is a no-op and the draw calls must
/// reproduce the committed masters byte-for-byte.
pub(crate) fn compose_on(
    img: &mut RgbImage,
    spec: &TemplateSpec,
    wallet: &Wallet,
    timestamp_utc: &str,
) -> Result<(), Error> {
    // Phase 1: heal. Only pixels that exactly match the marker color are
    // touched: QR regions heal to white (the code needs a light backing),
    // text regions to the majority color of the 1-px ring around the bbox
    // (a marker on a solid strip heals back to that strip's color). The
    // text regions' heal colors also pick the ink: dark ink on light
    // backgrounds, near-white on dark ones. On the built-in satoshi bill
    // (no marker pixels) every text background is light, so the classic
    // INK/BLACK — pinned by the masters — is what this chooses.
    let mut text_bg: [Rgb<u8>; 5] = [WHITE; 5];
    for (i, region) in ALL_REGIONS.iter().enumerate() {
        let Some(rect) = spec.region_rect(*region) else { continue };
        let heal = if region.is_qr() { WHITE } else { ring_majority_color(img, rect) };
        text_bg[i] = heal;
        let marker = Rgb(region.marker_rgb());
        for y in rect.y1..rect.y2.min(img.height()) {
            for x in rect.x1..rect.x2.min(img.width()) {
                if *img.get_pixel(x, y) == marker {
                    img.put_pixel(x, y, heal);
                }
            }
        }
    }
    let ink_on = |bg: Rgb<u8>, dark: Rgb<u8>| -> Rgb<u8> {
        let lum = 0.299 * bg.0[0] as f32 + 0.587 * bg.0[1] as f32 + 0.114 * bg.0[2] as f32;
        if lum >= 128.0 { dark } else { Rgb([245, 245, 245]) }
    };
    let privkey_ink = ink_on(text_bg[3], INK);
    let address_ink = ink_on(text_bg[2], INK);
    let timestamp_ink = ink_on(text_bg[4], BLACK);

    // Phase 2: draw. Same order and math as the classic satoshi overlay.
    bill::paste_qr_in_box(img, &qr::address_payload(&wallet.address), spec.address_qr.as_box())?;
    let sweep = qr::sweep_url(&wallet.bill_wif, wallet.variant);
    bill::paste_qr_in_box(img, &sweep, spec.privkey_qr.as_box())?;

    if wallet.is_tweaked {
        let label = "(tweaked)";
        let width = text::measure(BillFont::Condensed, 12.0, label);
        text::draw_text(
            img,
            BillFont::Condensed,
            12.0,
            spec.privkey_qr.x2 as f32 - width - 3.0,
            spec.privkey_qr.y2 as f32 - 3.0,
            INK,
            label,
        );
    }

    if let Some(rect) = spec.privkey_text {
        let (x1, y1, x2, y2) = rect.as_box();
        let (tw, th) = (x2 - x1, y2 - y1);
        let wif = wallet.bill_wif.as_str();
        if wallet.is_tweaked {
            let suffix = " (tweaked)";
            let suffix_w = text::measure(BillFont::Condensed, 12.0, suffix);
            let avail = tw - suffix_w - 2.0;
            let (size, width) = text::fit_to_box(BillFont::Mono, wif, avail, th, PRIVKEY_TEXT_MAX_PX);
            let suffix_x = x2 - suffix_w;
            let text_x = suffix_x - width - 2.0;
            let text_y = y1 + (th + size * 0.8) / 2.0;
            text::draw_text(img, BillFont::Mono, size, text_x, text_y, privkey_ink, wif);
            let suffix_y = y1 + (th + 12.0 * 0.8) / 2.0;
            text::draw_text(img, BillFont::Condensed, 12.0, suffix_x, suffix_y, privkey_ink, suffix);
        } else {
            let (size, width) = text::fit_to_box(BillFont::Mono, wif, tw, th, PRIVKEY_TEXT_MAX_PX);
            let text_x = x2 - width;
            let text_y = y1 + (th + size * 0.8) / 2.0;
            text::draw_text(img, BillFont::Mono, size, text_x, text_y, privkey_ink, wif);
        }
    }

    if let Some(rect) = spec.address_text {
        let (x1, y1, x2, y2) = rect.as_box();
        let (tw, th) = (x2 - x1, y2 - y1);
        let font = match wallet.variant {
            Variant::Segwit => BillFont::Mono,
            Variant::Taproot | Variant::TaprootBackup => BillFont::Condensed,
        };
        let (size, width) = text::fit_to_box(font, &wallet.address, tw, th, ADDRESS_TEXT_MAX_PX);
        let tx = x1 + (tw - width) / 2.0;
        let ty = y1 + (th + size * 0.8) / 2.0;
        text::draw_text(img, font, size, tx, ty, address_ink, &wallet.address);
    }

    if let Some(rect) = spec.timestamp {
        let (w, h) = (rect.width() as f32, rect.height() as f32);
        if h > w {
            // Vertical strip: rotated CCW, column right-aligned, bottom-anchored.
            let (size, _) =
                text::fit_to_box(BillFont::Mono, timestamp_utc, h, w, TIMESTAMP_MAX_PX);
            let px = rect.x2 as f32 - size;
            let py = rect.y2 as f32;
            text::draw_text_rotated_ccw(
                img,
                BillFont::Mono,
                size,
                px,
                py,
                size * 0.8,
                timestamp_ink,
                timestamp_utc,
            );
        } else {
            let (size, width) =
                text::fit_to_box(BillFont::Mono, timestamp_utc, w, h, TIMESTAMP_MAX_PX);
            let tx = rect.x1 as f32 + (w - width) / 2.0;
            let ty = rect.y1 as f32 + (h + size * 0.8) / 2.0;
            text::draw_text(img, BillFont::Mono, size, tx, ty, timestamp_ink, timestamp_utc);
        }
    }

    Ok(())
}

/// Majority color of the in-bounds 1-px ring just outside `rect`.
fn ring_majority_color(img: &RgbImage, rect: Rect) -> Rgb<u8> {
    let mut counts: HashMap<[u8; 3], u32> = HashMap::new();
    let (w, h) = img.dimensions();
    let mut tally = |x: i64, y: i64| {
        if x >= 0 && y >= 0 && (x as u32) < w && (y as u32) < h {
            *counts.entry(img.get_pixel(x as u32, y as u32).0).or_insert(0) += 1;
        }
    };
    for x in rect.x1 as i64 - 1..=rect.x2 as i64 {
        tally(x, rect.y1 as i64 - 1);
        tally(x, rect.y2 as i64);
    }
    for y in rect.y1 as i64..rect.y2 as i64 {
        tally(rect.x1 as i64 - 1, y);
        tally(rect.x2 as i64, y);
    }
    // Ties broken by color value: HashMap iteration order is random per
    // process, and composition must stay deterministic (a gradient ring
    // makes every count 1).
    counts
        .into_iter()
        .max_by_key(|(c, n)| (*n, *c))
        .map(|(c, _)| Rgb(c))
        .unwrap_or(WHITE)
}

/// The satoshi bill's region layout — the same rectangles the classic
/// hardcoded overlay used (timestamp strip chosen so the generic rotated
/// rule lands exactly on the classic placement). The masters pin this.
pub fn satoshi_spec() -> TemplateSpec {
    TemplateSpec {
        width: bill::BILL_WIDTH,
        height: bill::BILL_HEIGHT,
        address_qr: Rect { x1: 35, y1: 469, x2: 319, y2: 752 },
        privkey_qr: Rect { x1: 1525, y1: 40, x2: 1808, y2: 324 },
        address_text: Some(Rect { x1: 348, y1: 694, x2: 1148, y2: 751 }),
        privkey_text: Some(Rect { x1: 1100, y1: 2, x2: 1808, y2: 30 }),
        timestamp: Some(Rect { x1: 1813, y1: 560, x2: 1835, y2: 776 }),
    }
}

/// The satoshi artwork with the marker rectangles painted over it — the
/// design kit's working example template.
pub(crate) fn satoshi_marked_image() -> Result<RgbImage, Error> {
    let mut img = bill::decode_satoshi_art()?;
    let spec = satoshi_spec();
    for region in ALL_REGIONS {
        let rect = spec.region_rect(region).expect("satoshi spec has all regions");
        bill::fill_rect(&mut img, rect.x1, rect.y1, rect.x2, rect.y2, Rgb(region.marker_rgb()));
    }
    Ok(img)
}

/// A bill design that ships inside the app.
///
/// Three of them: the default `engraved` note plus two marker templates. The
/// split matters at compose time and nowhere else — see [`as_template`].
///
/// [`as_template`]: BuiltinDesign::as_template
pub struct BuiltinDesign {
    /// Stable key. Goes in logs and would go in any persisted selection;
    /// never a display string, so renaming a design cannot break either.
    pub id: &'static str,
    /// Picker label.
    pub name: &'static str,
    /// One-line description, shown under the label.
    pub blurb: &'static str,
    png: &'static [u8],
}

impl BuiltinDesign {
    /// The design's PNG.
    pub fn png(&self) -> &'static [u8] {
        self.png
    }

    /// `None` for the design that composes through [`bill::compose_bill`] —
    /// unmarked art, hardcoded [`satoshi_spec`], plus the banner pass that
    /// draws the motto and the live year. `Some(png)` for the marker
    /// templates, which go through [`compose_custom_bill`] exactly as a
    /// user's own PNG does, and therefore carry a baked motto and no year
    /// (that path runs no banner pass).
    pub fn as_template(&self) -> Option<&'static [u8]> {
        (!self.is_default()).then_some(self.png)
    }

    /// Whether this is the default design.
    pub fn is_default(&self) -> bool {
        self.id == DEFAULT_DESIGN_ID
    }
}

/// The design selected on a fresh install.
pub const DEFAULT_DESIGN_ID: &str = "engraved";

/// Every design that ships with the app, in picker order. All three share the
/// [`satoshi_spec`] layout, so a bill looks the same shape whichever is picked.
/// Generated by `assets/art/generate_bill_art.py`.
pub const BUILTIN_DESIGNS: &[BuiltinDesign] = &[
    BuiltinDesign {
        id: DEFAULT_DESIGN_ID,
        name: "Engraved",
        blurb: "Guilloche banknote",
        png: bill::TEMPLATE,
    },
    BuiltinDesign {
        id: "cypherpunk",
        name: "Cypherpunk",
        blurb: "Hex field, slate & orange",
        png: include_bytes!("../assets/templates/cypherpunk.png"),
    },
    BuiltinDesign {
        id: "ledger",
        name: "Ledger",
        blurb: "Flat, modern, ruled",
        png: include_bytes!("../assets/templates/ledger.png"),
    },
];

/// Look a design up by [`BuiltinDesign::id`].
pub fn builtin_design(id: &str) -> Option<&'static BuiltinDesign> {
    BUILTIN_DESIGNS.iter().find(|d| d.id == id)
}

/// The default design. Infallible: a missing entry is a build-time mistake,
/// and the picker has to render something.
pub fn default_design() -> &'static BuiltinDesign {
    &BUILTIN_DESIGNS[0]
}

/// The two files "Export design kit" writes (plus [`design_kit_readme`]).
pub struct DesignKit {
    /// Blank canvas with labeled marker rectangles at the satoshi positions.
    pub template_png: Vec<u8>,
    /// The satoshi artwork as a marker template — a working example.
    pub satoshi_example_png: Vec<u8>,
}

pub fn render_design_kit() -> Result<DesignKit, Error> {
    let spec = satoshi_spec();
    let mut blank = RgbImage::from_pixel(bill::BILL_WIDTH, bill::BILL_HEIGHT, WHITE);
    for region in ALL_REGIONS {
        let rect = spec.region_rect(region).expect("satoshi spec has all regions");
        bill::fill_rect(&mut blank, rect.x1, rect.y1, rect.x2, rect.y2, Rgb(region.marker_rgb()));
    }
    // Labels sit OUTSIDE their markers (text inside would break solidity).
    let label = |img: &mut RgbImage, x: f32, baseline: f32, s: &str| {
        text::draw_text(img, BillFont::Condensed, 18.0, x, baseline, INK, s);
    };
    label(&mut blank, 35.0, 30.0, "Bill design template \u{2014} move/resize the colored rectangles, draw your art around them. See README.txt.");
    label(&mut blank, 35.0, 774.0, "address QR (magenta, required)");
    label(&mut blank, 1525.0, 346.0, "private key QR (cyan, required)");
    label(&mut blank, 1100.0, 52.0, "private key text (red, optional)");
    label(&mut blank, 348.0, 690.0, "address text (green, optional)");
    let ts_label = "timestamp (blue, optional)";
    let ts_w = text::measure(BillFont::Condensed, 18.0, ts_label);
    label(&mut blank, 1813.0 - ts_w - 8.0, 578.0, ts_label);

    Ok(DesignKit {
        template_png: bill::encode_png(&blank)?,
        satoshi_example_png: bill::encode_png(&satoshi_marked_image()?)?,
    })
}

/// README.txt written next to the kit files.
pub fn design_kit_readme() -> &'static str {
    "DESIGN YOUR OWN BILL\n\
     ====================\n\
     A bill template is a plain PNG. Paint solid rectangles in these exact\n\
     colors to mark where the app should place each element:\n\
     \n\
       magenta (255,0,255)  address QR        REQUIRED\n\
       cyan    (0,255,255)  private key QR    REQUIRED\n\
       green   (0,255,0)    address text      optional\n\
       red     (255,0,0)    private key text  optional\n\
       blue    (0,0,255)    timestamp         optional\n\
     \n\
     yellow (255,255,0) is reserved for future use - avoid it in artwork.\n\
     \n\
     Rules:\n\
     - One solid, axis-aligned rectangle per color. Disable anti-aliasing\n\
       and paint fully opaque, or the marker may be rejected.\n\
     - QR rectangles: at least 64 px per side (200+ recommended for print).\n\
     - Text rectangles: at least 16 px per side; place them on a solid\n\
       background color (the marker is healed with the surrounding color,\n\
       and the text ink is dark on light backgrounds, light on dark ones).\n\
     - A timestamp rectangle taller than wide is drawn rotated, reading\n\
       bottom to top.\n\
     - Canvas can be any size up to 4096 x 4096.\n\
     \n\
     Files:\n\
     - template.png         blank canvas with the regions at the classic\n\
                            satoshi-bill positions, ready to restyle\n\
     - satoshi-example.png  the built-in satoshi artwork as a template,\n\
                            a working example of the marker system\n\
     \n\
     Put finished designs in /paper-wallets/templates on internal storage\n\
     or Airlock, then pick them under \"Bill design\" in the app.\n"
}
