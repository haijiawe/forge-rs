"""Draw the hearth mark, so the icon is reproducible instead of a mystery blob.

Run from the repository root:

    python apps/desktop/icons/make-icons.py

Writes `icon.ico` (16/32/48/64/128/256) and the four PNG sizes Tauri's config
lists. The shape is deliberately simple: a flame is unreadable at 16 px, so the
mark is an arch (the hearth opening) with an ember in it, drawn with plain
shapes and no fonts. Every number below is chosen for legibility at small sizes,
which is why the arch is thick and the ember is bigger than it looks like it
should be.
"""

from pathlib import Path

from PIL import Image, ImageDraw

HERE = Path(__file__).resolve().parent

# Palette: the dark slate of the app chrome, and two warm tones for the fire.
BACKGROUND = (26, 28, 33, 255)
ARCH = (232, 234, 238, 255)
EMBER = (255, 138, 61, 255)
EMBER_HOT = (255, 209, 138, 255)

SUPERSAMPLE = 4


def draw(size: int) -> Image.Image:
    """One square icon at `size` px, drawn large and scaled down."""
    scale = SUPERSAMPLE
    box = size * scale
    image = Image.new("RGBA", (box, box), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)

    # Rounded square: the window shape, and what makes the icon look like an
    # app rather than a logo dropped on a transparent field.
    radius = int(box * 0.22)
    draw.rounded_rectangle([0, 0, box - 1, box - 1], radius=radius, fill=BACKGROUND)

    # The hearth opening: a thick arch, open at the bottom, standing on the
    # baseline of the mark. The fire goes *inside* it -- an ember that overlaps
    # the arch reads as a face at small sizes.
    stroke = max(2, int(box * 0.075))
    left = int(box * 0.20)
    right = box - left
    top = int(box * 0.24)
    baseline = int(box * 0.74)
    draw.arc([left, top, right, baseline], start=180, end=360, fill=ARCH, width=stroke)
    # The two jambs, so the arch reads as a doorway and not as a rainbow.
    draw.line([left, top + (baseline - top) // 2, left, baseline], fill=ARCH, width=stroke)
    draw.line([right, top + (baseline - top) // 2, right, baseline], fill=ARCH, width=stroke)

    # The ember: a pointed teardrop, tip up, sitting in the doorway. A circle on
    # top of a triangle is the whole construction -- every curved flame I tried
    # first looked like a smudge below 32 px.
    cx = box // 2
    tip = int(box * 0.36)
    base = int(box * 0.70)
    body = int(box * 0.115)
    draw.ellipse([cx - body, base - body * 2, cx + body, base], fill=EMBER)
    draw.polygon(
        [
            (cx, tip),
            (cx + body * 0.86, base - body * 0.9),
            (cx - body * 0.86, base - body * 0.9),
        ],
        fill=EMBER,
    )
    # A hot core, so the flame has a centre once there are enough pixels.
    core = int(body * 0.45)
    draw.ellipse(
        [cx - core, base - core * 2 - body, cx + core, base - body],
        fill=EMBER_HOT,
    )

    return image.resize((size, size), Image.LANCZOS)


def main() -> None:
    sizes = [16, 32, 48, 64, 128, 256, 512]
    images = {size: draw(size) for size in sizes}

    # Tauri's default config names these four; ship what it asks for.
    images[32].save(HERE / "32x32.png")
    images[128].save(HERE / "128x128.png")
    images[256].save(HERE / "128x128@2x.png")
    images[512].save(HERE / "icon.png")

    # One ICO carrying every size, so Windows picks the right one for the
    # taskbar, the title bar and the file explorer instead of scaling one.
    ico_sizes = [16, 32, 48, 64, 128, 256]
    images[256].save(
        HERE / "icon.ico",
        format="ICO",
        sizes=[(size, size) for size in ico_sizes],
    )
    print(f"wrote {len(ico_sizes)} sizes into {HERE / 'icon.ico'}")
    for name in ("32x32.png", "128x128.png", "128x128@2x.png", "icon.png"):
        print(f"wrote {HERE / name}")


if __name__ == "__main__":
    main()