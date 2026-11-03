"""Draws the Inkveil app icon and writes crates/inkveil-app/assets/inkveil.ico.

The mark: a white fountain-pen nib on a deep ink-blue rounded square, with a frosted band across
its lower half (the "veil"). Drawn at 1024 px and downsampled for each icon size.

Run from the repository root:  python scripts/make-icon.py   (needs Pillow)
"""

from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

SIZE = 1024
INK = (24, 38, 74, 255)
INK_LIGHT = (47, 111, 219, 255)
WHITE = (255, 255, 255, 255)


def draw() -> Image.Image:
    img = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))

    # Background: rounded square with a vertical ink gradient.
    gradient = Image.new("RGBA", (SIZE, SIZE))
    gd = ImageDraw.Draw(gradient)
    for y in range(SIZE):
        t = y / (SIZE - 1)
        color = tuple(int(INK_LIGHT[i] * (1 - t) + INK[i] * t) for i in range(3)) + (255,)
        gd.line([(0, y), (SIZE, y)], fill=color)
    mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, SIZE - 1, SIZE - 1], radius=230, fill=255)
    img.paste(gradient, (0, 0), mask)

    # Nib: a pointed shield shape with a slit and a breather hole.
    nib = Image.new("RGBA", (SIZE, SIZE), (0, 0, 0, 0))
    nd = ImageDraw.Draw(nib)
    cx = SIZE // 2
    top, shoulder, tip = 190, 520, 860
    half = 230
    outline = [
        (cx - half + 40, top),
        (cx + half - 40, top),
        (cx + half, shoulder),
        (cx, tip),
        (cx - half, shoulder),
    ]
    nd.polygon(outline, fill=WHITE)
    # Slit and breather hole (cut out).
    nd.line([(cx, 470), (cx, tip - 40)], fill=(0, 0, 0, 0), width=26)
    nd.ellipse([cx - 52, 400, cx + 52, 504], fill=(0, 0, 0, 0))
    img.alpha_composite(nib)

    # The veil: a frosted, semi-transparent band across the lower half of the nib.
    band_top, band_bottom = 600, 760
    region = img.crop((0, band_top, SIZE, band_bottom)).filter(ImageFilter.GaussianBlur(9))
    frost = Image.new("RGBA", region.size, (255, 255, 255, 60))
    region = Image.alpha_composite(region, frost)
    band_mask = Image.new("L", (SIZE, SIZE), 0)
    ImageDraw.Draw(band_mask).rectangle([0, band_top, SIZE, band_bottom], fill=255)
    band_mask = Image.composite(band_mask, Image.new("L", (SIZE, SIZE), 0), mask)
    img.paste(region, (0, band_top), band_mask.crop((0, band_top, SIZE, band_bottom)))
    return img


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    out = root / "crates" / "inkveil-app" / "assets" / "inkveil.ico"
    png = root / "crates" / "inkveil-app" / "assets" / "images" / "inkveil.png"
    icon = draw()
    icon.resize((256, 256), Image.LANCZOS).save(png)
    icon.save(out, sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    print(f"wrote {out} and {png}")


if __name__ == "__main__":
    main()
