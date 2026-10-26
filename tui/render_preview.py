#!/usr/bin/env python3
"""Rasterise a snapshot from `cargo run --example snapshot` into a PNG.

This is deliberately a dumb renderer: it knows nothing about missions, diffs or memory. It
takes the cell grid the real views produced (`preview/*.json`: symbol, foreground,
background, weight per cell) and draws it with a monospace font. If the picture is wrong,
the code is wrong — there is no second source of truth to drift from.

    cargo run --example snapshot
    python3 render_preview.py               # preview/*.png
    cargo run --example frames
    python3 render_preview.py --gif         # preview/hole.gif
"""

import json
import pathlib
import sys

from PIL import Image, ImageDraw, ImageFont

FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf"
FONT_B = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono-Bold.ttf"
SIZE = 15
CHAR_W, CHAR_H = 9, 18
SCALE = 2
PAD = 16


# Braille dot order inside a cell (U+2800 block): bit n is dot n+1, and the dots are laid
# out two per row, four rows deep. A terminal draws them as dots; so must the preview, or the
# black hole comes out as rows of tofu boxes in a font with no braille.
BRAILLE_BITS = [
    (0x01, 0, 0), (0x08, 1, 0),
    (0x02, 0, 1), (0x10, 1, 1),
    (0x04, 0, 2), (0x20, 1, 2),
    (0x40, 0, 3), (0x80, 1, 3),
]


def draw_braille(draw, left, top, symbol, fill):
    """Draw the dots of a braille cell inside one character box."""
    bits = ord(symbol) - 0x2800
    if bits == 0xFF:
        # A full cell is a solid block in a terminal; draw it as one so the accretion disk
        # reads as a body rather than as a grid of dots.
        draw.rectangle([left, top, left + CHAR_W - 1, top + CHAR_H - 1], fill=fill)
        return
    # A cell is CHAR_W x CHAR_H; dots are a third of a column wide and a quarter-row tall,
    # with a little air so neighbouring dots stay distinct at preview resolution.
    dot_w = max(2, CHAR_W // 3)
    dot_h = max(2, CHAR_H // 5)
    gap_x = (CHAR_W - 2 * dot_w) / 2
    gap_y = (CHAR_H - 4 * dot_h) / 4
    for bit, col, row in BRAILLE_BITS:
        if not bits & bit:
            continue
        x0 = left + gap_x + col * dot_w
        y0 = top + gap_y + row * dot_h
        draw.rectangle([x0, y0, x0 + dot_w, y0 + dot_h], fill=fill)


def hexcolor(value, fallback):
    if not value or not value.startswith("#") or len(value) != 7:
        return fallback
    return tuple(int(value[i : i + 2], 16) for i in (1, 3, 5))


def render_image(path: pathlib.Path) -> Image.Image:
    data = json.loads(path.read_text())
    width, height = data["width"], data["height"]
    rows = data["cells"]

    img_w = width * CHAR_W + PAD * 2
    img_h = height * CHAR_H + PAD * 2
    # The page is the void, everywhere, in the picture as in the app.
    img = Image.new("RGB", (img_w, img_h), (5, 6, 10))
    d = ImageDraw.Draw(img)
    font = ImageFont.truetype(FONT, SIZE)
    font_b = ImageFont.truetype(FONT_B, SIZE)

    for y, row in enumerate(rows):
        cells = row
        # Background: one rectangle per run of equal colour, not one per cell.
        x = 0
        while x < len(cells):
            bg = cells[x][2]
            run = 1
            while x + run < len(cells) and cells[x + run][2] == bg:
                run += 1
            colour = hexcolor(bg, None)
            if colour is not None:
                d.rectangle(
                    [
                        PAD + x * CHAR_W,
                        PAD + y * CHAR_H,
                        PAD + (x + run) * CHAR_W - 1,
                        PAD + (y + 1) * CHAR_H - 1,
                    ],
                    fill=colour,
                )
            x += run

        # Text, per cell: the grid already decided every position.
        for x, (symbol, fg, _bg, bold) in enumerate(cells):
            if symbol in (" ", ""):
                continue
            fill = hexcolor(fg, (232, 228, 224))
            if "\u2800" <= symbol <= "\u28ff":
                draw_braille(d, PAD + x * CHAR_W, PAD + y * CHAR_H, symbol, fill)
                continue
            d.text(
                (PAD + x * CHAR_W, PAD + y * CHAR_H),
                symbol,
                font=font_b if bold == "1" else font,
                fill=hexcolor(fg, (232, 228, 224)),
            )

    return img.resize((img_w * SCALE // 2, img_h * SCALE // 2), Image.LANCZOS)


def render(path: pathlib.Path, out: pathlib.Path) -> tuple[int, int]:
    img = render_image(path)
    img.save(out, optimize=True)
    return img.size


def render_gif(frames: list[pathlib.Path], out: pathlib.Path, ms: int = 120) -> tuple[int, int, int]:
    """Join `preview/frames/*.json` into one animation.

    A static PNG can show that the black hole is drawn correctly; only a moving one can show
    that the accretion flow moves. One full swirl takes about 11 s at the shipped speed, so
    48 frames at 120 ms is the same motion at a little over twice the pace — fast enough to
    see the shear, slow enough to follow one filament round.
    """
    images = [render_image(path) for path in frames]
    images[0].save(
        out,
        save_all=True,
        append_images=images[1:],
        duration=ms,
        loop=0,
        optimize=True,
    )
    return (*images[0].size, len(images))


def main() -> int:
    here = pathlib.Path(__file__).parent
    preview = here / "preview"
    snapshots = sorted(preview.glob("*.json"))
    if not snapshots:
        print("no snapshots: run `cargo run --example snapshot` first", file=sys.stderr)
        return 1
    for path in snapshots:
        out = preview / f"{path.stem}.png"
        size = render(path, out)
        print(f"wrote preview/{out.name}  {size[0]}x{size[1]}")

    if "--gif" in sys.argv:
        frames = sorted((preview / "frames").glob("*.json"))
        if not frames:
            print("no frames: run `cargo run --example frames` first", file=sys.stderr)
            return 1
        out = preview / "hole.gif"
        width, height, count = render_gif(frames, out)
        print(f"wrote preview/{out.name}  {width}x{height}  {count} frames")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
