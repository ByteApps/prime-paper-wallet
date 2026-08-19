#!/usr/bin/env python3
"""Generate the bill artwork shipped in wallet-core/assets/.

Everything here is drawn from math and from the DejaVu fonts already vendored
beside it, so the output carries no third-party rights. It replaces the
previous "satoshi bill" art, which was a third-party illustration with no
license grant (and a real person's likeness).

Run from the repository root:

    python3 wallet-core/assets/art/generate_bill_art.py

Outputs, all 1843x784:

    ../bill_template.png          the built-in bill (Engraved), UNMARKED --
                                  compose_bill draws onto it using the
                                  hardcoded template::satoshi_spec rectangles
    ../templates/cypherpunk.png   marker templates for the custom-template
    ../templates/ledger.png       engine (template::detect)

Contract with wallet-core/src/template.rs, which the art must satisfy:

  * QR rects get a QR pasted on top -- keep a light panel under them.
  * Text rects take their ink from the 1-px ring AROUND the rect
    (ring_majority_color; luminance >= 128 -> dark ink, else near-white), so
    each text rect sits on a solid, uniform strip.
  * The two banner boxes are overpainted at compose time with BANNER_COLOR
    and the motto/year in BANNER_TEXT_COLOR, so the art puts a matching band
    there and leaves the lettering to the composer.
  * For the MARKED templates each marker must be one solid, opaque,
    axis-aligned rectangle and the only pixels of that exact color.

Regenerating the art changes composed output, so the golden masters in
wallet-core/tests/fixtures/ must be regenerated with it (see
examples/render_samples.rs).
"""
import math
import os
import random

from PIL import Image, ImageDraw, ImageFont

W, H = 1843, 784
SS = 2  # supersample, downsampled at the end for anti-aliasing

# template::satoshi_spec()
REGIONS = {
    "address_qr":   (35, 469, 319, 752),
    "privkey_qr":   (1525, 40, 1808, 324),
    "address_text": (348, 694, 1148, 751),
    "privkey_text": (1100, 2, 1808, 30),
    "timestamp":    (1813, 560, 1835, 776),
}
MARKER_RGB = {
    "address_qr": (255, 0, 255), "privkey_qr": (0, 255, 255),
    "address_text": (0, 255, 0), "privkey_text": (255, 0, 0),
    "timestamp": (0, 0, 255),
}
BANNER_BAND = (1078, 298, 1429, 346)

HERE = os.path.dirname(os.path.abspath(__file__))
ASSETS = os.path.dirname(HERE)
MONO = os.path.join(ASSETS, "DejaVuSansMono.ttf")
COND = os.path.join(ASSETS, "DejaVuSansCondensed.ttf")


def s(v):
    return int(round(v * SS))


def font(path, px):
    return ImageFont.truetype(path, int(px * SS))


def text_w(d, txt, f):
    return d.textbbox((0, 0), txt, font=f)[2]


def _one(d, xy, txt, f, fill, bold):
    x, y = xy
    if bold:
        r = bold * SS
        n = max(1, int(r))
        for dx in range(-n, n + 1):
            for dy in range(-n, n + 1):
                if dx * dx + dy * dy <= r * r:
                    d.text((x + dx, y + dy), txt, font=f, fill=fill)
    else:
        d.text((x, y), txt, font=f, fill=fill)


def text(d, xy, txt, f, fill, bold=0.0, anchor=None, tracking=None):
    x, y = xy
    if tracking:
        total = sum(text_w(d, c, f) for c in txt) + tracking * SS * (len(txt) - 1)
        if anchor == "m":
            x -= total / 2
        for c in txt:
            _one(d, (x, y), c, f, fill, bold)
            x += text_w(d, c, f) + tracking * SS
        return
    if anchor == "m":
        x -= text_w(d, txt, f) / 2
    _one(d, (x, y), txt, f, fill, bold)


def guilloche(d, cx, cy, R, r, dist, color, width=0.5, phase=0.0):
    """Hypotrochoid -- the interference figure a rose-engine lathe cuts into
    an engraved note. Closed after r/gcd(R,r) turns."""
    turns = int(r / math.gcd(int(R), int(r)))
    steps = turns * 720
    k = (R - r) / r
    pts = []
    for i in range(steps + 1):
        t = phase + 2 * math.pi * turns * i / steps
        pts.append((cx + ((R - r) * math.cos(t) + dist * math.cos(k * t)) * SS,
                    cy + ((R - r) * math.sin(t) - dist * math.sin(k * t)) * SS))
    d.line(pts, fill=color, width=max(1, int(width * SS)), joint="curve")


def rosette(d, cx, cy, R, color, rings, width=0.5):
    for (r, dist, phase) in rings:
        guilloche(d, cx, cy, R, r, dist, color, width=width, phase=phase)


def btc_mark(img, cx, cy, size, color, weight=0.045, tilt=0.0):
    """The bitcoin mark: a B with two vertical bars.

    The bars are drawn only as the stubs above and below the glyph -- running
    them through the body puts a stripe across the bowls' white counters,
    which is not what the symbol looks like.

    `tilt` leans the mark clockwise. The official orange-disc logo is set at
    roughly 14 degrees; upright reads as wrong wherever we echo that logo.
    """
    pad_px = int(size * SS)
    layer = Image.new("RGBA", (pad_px * 2, pad_px * 2), (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    lx, ly = pad_px, pad_px

    f = font(COND, size)
    bb = ld.textbbox((0, 0), "B", font=f)
    bw, bh = bb[2] - bb[0], bb[3] - bb[1]
    x = lx - bw / 2 - bb[0]
    y = ly - bh / 2 - bb[1]
    pad = size * weight * SS
    top, bot = y + bb[1] - pad, y + bb[1] + bh + pad
    bar_w = max(2, int(size * 0.075 * SS))
    over = size * 0.15 * SS
    for frac in (0.30, 0.58):
        bx = x + bb[0] + bw * frac
        ld.rectangle([bx - bar_w / 2, top - over, bx + bar_w / 2, top + pad], fill=color)
        ld.rectangle([bx - bar_w / 2, bot - pad, bx + bar_w / 2, bot + over], fill=color)
    _one(ld, (x, y), "B", f, color, size * weight)

    if tilt:
        layer = layer.rotate(-tilt, resample=Image.BICUBIC, center=(lx, ly))
    img.paste(layer, (int(cx - lx), int(cy - ly)), layer)


def paper(base, seed=7):
    """Paper ground.

    Deliberately FLAT. An earlier pass sprinkled per-pixel fibre noise here;
    it looked marginally warmer and cost ~1 MB, because random per-pixel
    dither defeats PNG's filters almost completely. This asset is
    include_bytes!'d into an app that runs on a memory-constrained device,
    so a megabyte of invisible noise is not a trade worth making. The
    guilloche supplies the texture instead.
    """
    return Image.new("RGB", (W * SS, H * SS), base)


def window(d, rect, pad, fill, border, bw=3, inner=None):
    x1, y1, x2, y2 = rect
    box = [s(x1 - pad), s(y1 - pad), s(x2 + pad), s(y2 + pad)]
    d.rectangle(box, fill=fill)
    d.rectangle(box, outline=border, width=int(bw * SS))
    if inner:
        d.rectangle([box[0] + s(5), box[1] + s(5), box[2] - s(5), box[3] - s(5)],
                    outline=inner, width=int(1 * SS))


def strip(d, rect, px, py, fill):
    x1, y1, x2, y2 = rect
    d.rectangle([s(x1 - px), s(y1 - py), s(x2 + px), s(y2 + py)], fill=fill)


def save(img, path, markers=False):
    """Downsample, then write an INDEXED PNG.

    Indexed matters: the art is include_bytes!'d into an app that runs on a
    memory-constrained device. As 24-bit RGB the guilloche costs ~1.35 MB,
    because thousands of thin anti-aliased curves defeat PNG's filters. The
    same pixels as a 256-entry palette cost ~0.55 MB -- in line with the
    artwork this replaces -- for a mean channel error of 0.35.

    Marker colors are placed in the palette EXPLICITLY rather than hoped for
    out of the quantizer, because template::detect matches them pixel-exactly
    and a near-miss is an undetectable template.
    """
    out = img.resize((W, H), Image.LANCZOS)
    if markers:
        pal_img = out.quantize(colors=256 - len(MARKER_RGB), method=Image.MEDIANCUT,
                               dither=Image.Dither.NONE)
        base = 256 - len(MARKER_RGB)
        pal = pal_img.getpalette()[: base * 3]
        for rgb in MARKER_RGB.values():
            pal.extend(rgb)
        pal_img.putpalette(pal)
        d = ImageDraw.Draw(pal_img)
        for i, (key, (x1, y1, x2, y2)) in enumerate(REGIONS.items()):
            d.rectangle([x1, y1, x2 - 1, y2 - 1], fill=base + i)
        out = pal_img
    else:
        out = out.quantize(colors=256, method=Image.MEDIANCUT, dither=Image.Dither.NONE)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    out.save(path, optimize=True)
    print(f"wrote {os.path.relpath(path, ASSETS)}  {out.size}  "
          f"{os.path.getsize(path) / 1024:.0f} KB")


# --------------------------------------------------------------------------
# Engraved -- the built-in bill
# --------------------------------------------------------------------------
def engraved():
    IVORY, PANEL = (244, 237, 225), (250, 246, 238)
    TEAL, ORANGE, CREAM = (16, 58, 74), (191, 108, 22), (253, 229, 167)
    F = 30  # frame inset: clears the WIF strip (y<=30) and timestamp (x>=1813)

    img = paper(IVORY)
    lay = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    g = ImageDraw.Draw(lay)
    rosette(g, s(706), s(392), 268, TEAL + (86,),
            [(37, 96, 0.0), (31, 78, 0.55), (29, 112, 1.1)])
    rosette(g, s(1300), s(400), 196, ORANGE + (86,), [(23, 62, 0.0), (19, 48, 0.7)])
    rosette(g, s(240), s(560), 150, TEAL + (58,), [(17, 42, 0.3)])
    rosette(g, s(1648), s(596), 116, TEAL + (52,), [(13, 34, 0.9)])
    for i in range(30):
        pts = [(s(x), s(392 + 235 * math.sin(x / 165 + i * 0.21) * math.cos(x / 1100)))
               for x in range(0, W + 8, 6)]
        g.line(pts, fill=TEAL + (18,), width=int(0.6 * SS), joint="curve")
    img = Image.alpha_composite(img.convert("RGBA"), lay).convert("RGB")
    d = ImageDraw.Draw(img)

    d.rectangle([s(F), s(F), s(W - F), s(H - F)], outline=TEAL, width=int(3 * SS))
    d.rectangle([s(F + 8), s(F + 8), s(W - F - 8), s(H - F - 8)], outline=ORANGE, width=int(1 * SS))
    d.rectangle([s(F + 13), s(F + 13), s(W - F - 13), s(H - F - 13)], outline=TEAL, width=int(1 * SS))

    MX, MY, MR = 706, 388, 188
    d.ellipse([s(MX - MR), s(MY - MR), s(MX + MR), s(MY + MR)], fill=PANEL)
    med = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    rosette(ImageDraw.Draw(med), s(MX), s(MY), 150, TEAL + (105,), [(19, 46, 0.0), (17, 38, 0.8)])
    img = Image.alpha_composite(img.convert("RGBA"), med).convert("RGB")
    d = ImageDraw.Draw(img)
    for i in range(160):  # engine-turned rim
        a = 2 * math.pi * i / 160
        r0, r1 = (s(156), s(172)) if i % 4 else (s(152), s(176))
        d.line([(s(MX) + math.cos(a) * r0, s(MY) + math.sin(a) * r0),
                (s(MX) + math.cos(a) * r1, s(MY) + math.sin(a) * r1)],
               fill=TEAL, width=int(0.9 * SS))
    d.ellipse([s(MX - MR), s(MY - MR), s(MX + MR), s(MY + MR)], outline=TEAL, width=int(3 * SS))
    d.ellipse([s(MX - 146), s(MY - 146), s(MX + 146), s(MY + 146)], outline=ORANGE, width=int(1 * SS))
    btc_mark(img, s(MX), s(MY), 232, TEAL)
    d = ImageDraw.Draw(img)

    text(d, (s(72), s(74)), "BITCOIN", font(COND, 92), TEAL, bold=1.5)
    text(d, (s(76), s(176)), "BEARER NOTE", font(COND, 26), ORANGE, tracking=8)
    d.line([(s(76), s(214)), (s(452), s(214))], fill=ORANGE, width=int(2 * SS))
    text(d, (s(76), s(226)), "PAY TO THE HOLDER OF THIS KEY", font(COND, 17), TEAL, tracking=2)
    text(d, (s(76), s(254)), "MAINNET · GENERATED OFFLINE · SINGLE USE", font(COND, 14),
         (120, 116, 104), tracking=1)

    text(d, (s(1300), s(452)), "ONE KEY", font(COND, 46), TEAL, bold=0.8, anchor="m", tracking=3)
    text(d, (s(1300), s(506)), "ONE BEARER", font(COND, 22), ORANGE, anchor="m", tracking=5)
    d.line([(s(1160), s(542)), (s(1440), s(542))], fill=ORANGE, width=int(1 * SS))
    text(d, (s(1300), s(556)), "SWEEP IT · THEN KEEP THE PAPER", font(COND, 14),
         (120, 116, 104), anchor="m")

    for (cx, cy) in ((150, 660), (1666, 632)):
        d.ellipse([s(cx - 46), s(cy - 46), s(cx + 46), s(cy + 46)], outline=ORANGE, width=int(2 * SS))
        d.ellipse([s(cx - 40), s(cy - 40), s(cx + 40), s(cy + 40)], outline=TEAL, width=int(1 * SS))
        btc_mark(img, s(cx), s(cy), 58, ORANGE)
    d = ImageDraw.Draw(img)

    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])], fill=CREAM)
    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])],
                outline=TEAL, width=int(2 * SS))

    window(d, REGIONS["address_qr"], 15, (255, 255, 255), TEAL, 3, ORANGE)
    window(d, REGIONS["privkey_qr"], 15, (255, 255, 255), TEAL, 3, ORANGE)
    strip(d, REGIONS["address_text"], 12, 9, PANEL)
    d.rectangle([s(336), s(685), s(1160), s(760)], outline=TEAL, width=int(2 * SS))
    d.rectangle([s(341), s(690), s(1155), s(755)], outline=ORANGE, width=int(1 * SS))
    strip(d, REGIONS["privkey_text"], 10, 2, PANEL)
    strip(d, REGIONS["timestamp"], 5, 10, PANEL)

    # 16px, not 18: at 18 the label ran under the frame's inner rule
    text(d, (s(177), s(437)), "PUBLIC KEY · LOAD & VERIFY", font(COND, 16), TEAL, anchor="m", tracking=1)
    text(d, (s(1666), s(348)), "PRIVATE KEY · SPEND & WITHDRAW", font(COND, 16), TEAL, anchor="m", tracking=1)
    text(d, (s(1666), s(374)), "KEEP SEALED UNTIL SWEPT", font(COND, 13), ORANGE, anchor="m")
    return img


# --------------------------------------------------------------------------
# Cypherpunk
# --------------------------------------------------------------------------
def cypherpunk():
    BONE, SLATE = (240, 240, 238), (22, 28, 36)
    ORANGE, MUTED = (232, 137, 24), (120, 128, 138)
    F = 30

    img = paper(BONE, seed=13)
    lay = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    g = ImageDraw.Draw(lay)
    f = font(MONO, 12)
    rnd = random.Random(11)
    for col in range(46, W - 40, 22):
        for row in range(46, H - 40, 19):
            if rnd.random() > 0.5:
                continue
            g.text((s(col), s(row)), rnd.choice("0123456789abcdef"), font=f, fill=SLATE + (26,))
    rosette(g, s(706), s(392), 250, ORANGE + (70,), [(29, 74, 0.0), (23, 58, 0.6)], width=0.6)
    img = Image.alpha_composite(img.convert("RGBA"), lay).convert("RGB")
    d = ImageDraw.Draw(img)

    d.rectangle([s(F), s(F), s(W - F), s(H - F)], outline=SLATE, width=int(3 * SS))
    d.rectangle([s(F + 9), s(F + 9), s(W - F - 9), s(H - F - 9)], outline=ORANGE, width=int(1 * SS))

    MX, MY, MR = 706, 388, 176
    hexpts = [(s(MX) + math.cos(math.pi / 6 + i * math.pi / 3) * s(MR),
               s(MY) + math.sin(math.pi / 6 + i * math.pi / 3) * s(MR)) for i in range(6)]
    d.polygon(hexpts, fill=SLATE)
    d.line(hexpts + [hexpts[0]], fill=ORANGE, width=int(3 * SS), joint="curve")
    btc_mark(img, s(MX), s(MY), 214, ORANGE)
    d = ImageDraw.Draw(img)

    text(d, (s(72), s(78)), "BITCOIN", font(COND, 88), SLATE, bold=1.5)
    text(d, (s(76), s(176)), "BEARER  KEY", font(COND, 25), ORANGE, tracking=9)
    d.line([(s(76), s(214)), (s(452), s(214))], fill=ORANGE, width=int(2 * SS))
    for i, line in enumerate(("> whoever holds the key holds the coin",
                              "> mainnet · generated offline · single use",
                              "> sweep it, then keep the paper")):
        text(d, (s(76), s(228 + i * 26)), line, font(MONO, 15), MUTED)

    text(d, (s(1300), s(452)), "ONE KEY", font(COND, 44), SLATE, bold=0.8, anchor="m", tracking=3)
    text(d, (s(1300), s(504)), "ONE BEARER", font(COND, 21), ORANGE, anchor="m", tracking=5)

    # compose_custom_bill runs no banner pass, so the motto is baked in here
    # (and the year is dropped -- static art cannot carry a live one)
    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])],
                fill=(253, 229, 167))
    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])],
                outline=SLATE, width=int(2 * SS))
    text(d, (s((BANNER_BAND[0] + BANNER_BAND[2]) / 2), s(BANNER_BAND[1] + 12)),
         "VIRES IN NUMERIS", font(COND, 22), SLATE, anchor="m", tracking=3)

    for key in ("address_qr", "privkey_qr"):
        x1, y1, x2, y2 = REGIONS[key]
        p = 16
        d.rectangle([s(x1 - p), s(y1 - p), s(x2 + p), s(y2 + p)], fill=SLATE)
        for (cx, cy, dx, dy) in ((x1 - p, y1 - p, 1, 1), (x2 + p, y1 - p, -1, 1),
                                 (x1 - p, y2 + p, 1, -1), (x2 + p, y2 + p, -1, -1)):
            d.line([(s(cx), s(cy)), (s(cx + dx * 40), s(cy))], fill=ORANGE, width=int(5 * SS))
            d.line([(s(cx), s(cy)), (s(cx), s(cy + dy * 40))], fill=ORANGE, width=int(5 * SS))
        window(d, REGIONS[key], 7, (255, 255, 255), (255, 255, 255), 1)

    text(d, (s(177), s(432)), "PUBLIC KEY · LOAD & VERIFY", font(COND, 16), SLATE, anchor="m", tracking=1)
    text(d, (s(1666), s(352)), "PRIVATE KEY · SPEND & WITHDRAW", font(COND, 16), SLATE, anchor="m", tracking=1)

    # dark strip -> the engine's luminance rule picks near-white ink here
    strip(d, REGIONS["address_text"], 13, 10, SLATE)
    strip(d, REGIONS["privkey_text"], 10, 2, (250, 250, 248))
    strip(d, REGIONS["timestamp"], 5, 10, (250, 250, 248))
    return img


# --------------------------------------------------------------------------
# Ledger
# --------------------------------------------------------------------------
def ledger():
    BONE, INK = (248, 246, 242), (28, 32, 38)
    ORANGE, RULE = (238, 143, 30), (222, 217, 208)
    F = 30

    img = Image.new("RGB", (W * SS, H * SS), BONE)
    d = ImageDraw.Draw(img)
    for y in range(F + 30, H - F, 34):
        d.line([(s(F), s(y)), (s(W - F), s(y))], fill=RULE, width=int(1 * SS))
    d.rectangle([s(F), s(F), s(F + 14), s(H - F)], fill=ORANGE)

    lay = Image.new("RGBA", (W * SS, H * SS), (0, 0, 0, 0))
    rosette(ImageDraw.Draw(lay), s(1280), s(392), 210, ORANGE + (56,),
            [(23, 60, 0.0), (19, 46, 0.7)], width=0.6)
    img = Image.alpha_composite(img.convert("RGBA"), lay).convert("RGB")
    d = ImageDraw.Draw(img)
    d.rectangle([s(F), s(F), s(W - F), s(H - F)], outline=INK, width=int(2 * SS))

    # echoes the official orange-disc logo, so the mark carries its 14-degree lean
    d.ellipse([s(706 - 168), s(392 - 168), s(706 + 168), s(392 + 168)], fill=ORANGE)
    btc_mark(img, s(706), s(392), 224, (255, 255, 255), tilt=14)
    d = ImageDraw.Draw(img)

    text(d, (s(96), s(84)), "BITCOIN", font(COND, 86), INK, bold=1.6)
    text(d, (s(100), s(182)), "BEARER NOTE", font(COND, 25), ORANGE, tracking=9)
    d.line([(s(100), s(220)), (s(470), s(220))], fill=INK, width=int(2 * SS))
    text(d, (s(100), s(232)), "PAY TO THE HOLDER OF THIS KEY", font(COND, 16), (110, 112, 118), tracking=2)

    text(d, (s(1300), s(440)), "ONE KEY", font(COND, 46), INK, bold=0.8, anchor="m", tracking=3)
    text(d, (s(1300), s(496)), "ONE BEARER", font(COND, 21), ORANGE, anchor="m", tracking=5)
    d.line([(s(1150), s(534)), (s(1450), s(534))], fill=RULE, width=int(2 * SS))
    text(d, (s(1300), s(548)), "MAINNET · OFFLINE · SINGLE USE", font(COND, 14), (140, 140, 145), anchor="m")

    # baked motto -- see the note in cypherpunk()
    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])],
                fill=(255, 255, 255))
    d.rectangle([s(BANNER_BAND[0]), s(BANNER_BAND[1]), s(BANNER_BAND[2]), s(BANNER_BAND[3])],
                outline=RULE, width=int(2 * SS))
    text(d, (s((BANNER_BAND[0] + BANNER_BAND[2]) / 2), s(BANNER_BAND[1] + 12)),
         "VIRES IN NUMERIS", font(COND, 22), ORANGE, anchor="m", tracking=3)

    window(d, REGIONS["address_qr"], 18, (255, 255, 255), INK, 2)
    window(d, REGIONS["privkey_qr"], 18, (255, 255, 255), INK, 2)
    text(d, (s(177), s(434)), "PUBLIC KEY · LOAD & VERIFY", font(COND, 16), INK, anchor="m", tracking=1)
    text(d, (s(1666), s(356)), "PRIVATE KEY · SPEND & WITHDRAW", font(COND, 16), INK, anchor="m", tracking=1)

    strip(d, REGIONS["address_text"], 14, 12, (255, 255, 255))
    d.rectangle([s(334), s(682), s(1162), s(763)], outline=INK, width=int(2 * SS))
    strip(d, REGIONS["privkey_text"], 10, 2, (255, 255, 255))
    strip(d, REGIONS["timestamp"], 5, 10, (255, 255, 255))
    return img


if __name__ == "__main__":
    save(engraved(), os.path.join(ASSETS, "bill_template.png"))
    save(cypherpunk(), os.path.join(ASSETS, "templates", "cypherpunk.png"), markers=True)
    save(ledger(), os.path.join(ASSETS, "templates", "ledger.png"), markers=True)
