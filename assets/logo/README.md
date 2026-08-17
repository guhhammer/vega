# The Vega mark

A four-pointed star, drawn as the space four adjoining circles leave between
them. That is why its edges curve *inward* rather than merely rounding off: each
edge is a circular arc centred on the corner it faces, and the arcs meet exactly
at the four points, so the outline closes with no join to smooth over. Inside
sits the same shape at a fifth the size — a glint, not a second logo.

The gradient runs bottom-left to top-right, `#d0c9ca` into `#797979`, at a true
45°. The inner star runs the same axis reversed, which is what keeps it legible
against the outer one at both ends instead of matching the tone underneath it
and disappearing.

## Files

| File | Use |
| --- | --- |
| `vega-mark.svg` | The mark alone. The source of truth for the shape. |
| `vega-lockup.svg` | Mark and word on one line. The word is live text — see below. |
| `png/vega-mark-*.png` | 16 – 1024px, transparent. What to drop into anything that wants a bitmap. |
| `png/vega-lockup-on-dark.png` | Lockup with light ink, for dark backgrounds. |
| `png/vega-lockup-on-light.png` | Lockup with dark ink, for light backgrounds. |
| `vega-icon-master.png` | 1024px with a 12% margin. What the application icons are generated from. |
| `render.py` | Regenerates every PNG above. |

## Regenerating

```bash
python3 assets/logo/render.py          # every PNG in this directory
cd app && npm run tauri icon -- ../assets/logo/vega-icon-master.png
```

The second command rewrites `app/src-tauri/icons/`, which is where the desktop
and Android builds get their icons. It also emits an iOS set and a set of
Windows Store logos; both are deleted, because Vega ships neither an iOS build
nor an MSIX package.

`render.py` draws the geometry a second time in cairo rather than converting the
SVG, because the SVG rasterisers that tend to be installed are not to be trusted
with a gradient — ImageMagick's built-in renderer silently drops them and hands
back a silhouette. The constants at the top of the script and the numbers in
`vega-mark.svg` are the same numbers, and have to be changed together.

## Two notes on use

**The margin is not baked in.** The mark runs to the edge of its viewBox, so
whatever places it decides its own clear space. The one exception is
`vega-icon-master.png`, which has a margin because every platform crops or masks
application icons and a shape that reaches the edge loses its points.

**The lockup's word is live text.** It renders with Quicksand if that is
installed and falls back through other geometric sans faces to plain
`sans-serif`. For anywhere the result has to be identical on every machine, use
the PNG — the face is baked into it.
