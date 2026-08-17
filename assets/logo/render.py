#!/usr/bin/env python3
"""Render the Vega mark to PNG.

`vega-mark.svg` is the source of truth for the shape; this draws the same
geometry with cairo so that the raster files can be regenerated on a machine
with no SVG rasteriser worth trusting. ImageMagick's built-in renderer is one of
those — it drops the gradients and hands back a silhouette — so the two are kept
deliberately in step here rather than converted.

    python3 assets/logo/render.py

Writes every size under assets/logo/png/, plus the 1024px master that the app
icons are generated from.

Keep the numbers below identical to the SVG. If one changes, change both: a
logo that differs between its vector and raster forms is worse than either.
"""

import math
import os

import cairo

# The star's tips sit on the four compass points of a 512 box, and every edge is
# an arc of this radius centred on the corner it faces — which is what makes the
# edges curve inward instead of merely rounding off. See vega-mark.svg.
SIZE = 512.0
R = SIZE / 2

# The inner star, as a fraction of the outer one. Small enough to read as a
# glint inside the mark rather than as a second logo arguing with it.
INNER = 0.215

# Bottom-left to top-right, light into dark.
LIGHT = (0xD0 / 255, 0xC9 / 255, 0xCA / 255)
DARK = (0x79 / 255, 0x79 / 255, 0x79 / 255)

# The same axis reversed, for the inner star. Reversed rather than recoloured:
# it stays in the one palette, and running the gradient the other way is what
# keeps it legible against the outer star at both ends instead of matching the
# tone underneath it and vanishing.
SPARK_DARK = (0x6F / 255, 0x6F / 255, 0x6F / 255)
SPARK_LIGHT = (0xEF / 255, 0xEB / 255, 0xEC / 255)

HERE = os.path.dirname(os.path.abspath(__file__))


def star(ctx, cx, cy, r):
    """The four-pointed star, as four arcs centred on the corners of a square.

    Cairo's angles run clockwise on a y-down surface, so each arc goes from the
    tip before it to the tip after it in increasing angle. The arcs meet exactly
    at the tips, so the path closes with no seam and no join to smooth over.
    """
    ctx.new_sub_path()
    ctx.arc(cx + r, cy - r, r, math.pi / 2, math.pi)  # right tip → top tip
    ctx.arc(cx - r, cy - r, r, 0, math.pi / 2)  # top → left
    ctx.arc(cx - r, cy + r, r, -math.pi / 2, 0)  # left → bottom
    ctx.arc(cx + r, cy + r, r, math.pi, 3 * math.pi / 2)  # bottom → right
    ctx.close_path()


def axis_gradient(cx, cy, r, start, end):
    """A 45° gradient across the square a star of this radius occupies.

    User-space rather than across the bounding box, so the angle is exactly 45°
    and the inner and outer stars are lit from the same direction.
    """
    gradient = cairo.LinearGradient(cx - r, cy + r, cx + r, cy - r)
    gradient.add_color_stop_rgb(0, *start)
    gradient.add_color_stop_rgb(1, *end)
    return gradient


def draw(size, margin=0.0):
    """One square surface with the mark on it, transparent behind.

    `margin` is the fraction of the edge left empty on each side. Zero for a
    plain asset; an application icon wants some, because every platform crops or
    masks icons and a shape that runs to the edge loses its points.
    """
    surface = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
    ctx = cairo.Context(surface)

    centre = size / 2
    r = centre * (1 - margin)

    star(ctx, centre, centre, r)
    ctx.set_source(axis_gradient(centre, centre, r, LIGHT, DARK))
    ctx.fill()

    inner = r * INNER * 2
    star(ctx, centre, centre, inner)
    ctx.set_source(axis_gradient(centre, centre, inner, SPARK_DARK, SPARK_LIGHT))
    ctx.fill()

    return surface


# The wordmark's face. Geometric rather than humanist, to sit with a mark built
# out of circles, and installed on the machine that renders this — a lockup is a
# raster asset, so the font is baked in and nothing downstream has to have it.
# The SVG names a stack instead, for anyone editing it.
WORDMARK_FONT = "Quicksand"


def lockup(ink, height=320):
    """The mark and the word, on one line, with nothing behind them.

    Two versions exist rather than one: a transparent lockup still has to
    commit to an ink colour, and one that reads on a dark background is
    invisible on a light one.
    """
    mark = int(height * 0.62)
    gap = int(height * 0.20)
    size = height * 0.46

    # Measured before the real surface is made, since the width depends on how
    # wide the word turns out to be in whatever face was found.
    probe = cairo.Context(cairo.ImageSurface(cairo.FORMAT_ARGB32, 1, 1))
    probe.select_font_face(WORDMARK_FONT, cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
    probe.set_font_size(size)
    extents = probe.text_extents("Vega")

    # Letter-spaced to match the wordmark in the application's sidebar, which
    # means drawing it a character at a time — cairo's toy text API has no
    # tracking of its own.
    # The advance of each glyph, not the ink extents: the word is drawn a
    # character at a time and the surface has to be as wide as the pen travels,
    # side bearings included, or the last letter sits on the edge.
    tracking = size * 0.06
    width = math.ceil(
        sum(probe.text_extents(ch).x_advance for ch in "Vega") + tracking * 3
    )

    # Clear space, so the mark's points and the last letter are not flush with
    # the edge of the file — whatever this gets dropped into will not add any.
    pad = int(height * 0.08)

    surface = cairo.ImageSurface(
        cairo.FORMAT_ARGB32, pad * 2 + mark + gap + width, height
    )
    ctx = cairo.Context(surface)

    star_surface = draw(mark)
    ctx.set_source_surface(star_surface, pad, (height - mark) / 2)
    ctx.paint()

    ctx.select_font_face(WORDMARK_FONT, cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_BOLD)
    ctx.set_font_size(size)
    ctx.set_source_rgb(*ink)

    x = pad + mark + gap
    baseline = height / 2 + extents.height / 2
    for ch in "Vega":
        ctx.move_to(x, baseline)
        ctx.show_text(ch)
        x += ctx.text_extents(ch).x_advance + tracking

    return surface


def main():
    out = os.path.join(HERE, "png")
    os.makedirs(out, exist_ok=True)

    for size in (1024, 512, 256, 128, 64, 32, 16):
        draw(size).write_to_png(os.path.join(out, f"vega-mark-{size}.png"))

    # What `tauri icon` is pointed at. The margin is here rather than in the
    # generator because every platform mask assumes the artwork has its own.
    draw(1024, margin=0.12).write_to_png(os.path.join(HERE, "vega-icon-master.png"))

    # Named for the background they go on, not for the colour of the ink, which
    # is the way round somebody placing one is thinking about it.
    lockup((0.86, 0.89, 0.89)).write_to_png(os.path.join(out, "vega-lockup-on-dark.png"))
    lockup((0.05, 0.07, 0.08)).write_to_png(os.path.join(out, "vega-lockup-on-light.png"))

    print(f"wrote {out}/ and vega-icon-master.png")


if __name__ == "__main__":
    main()
