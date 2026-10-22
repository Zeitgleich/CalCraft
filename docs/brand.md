# Brand

The CalCraft mark ("Blatt und Funke") is a rounded calendar page with two
binder rings and a white four-point spark: a calendar, made by hand.

All files live in [`assets/brand/`](../assets/brand/):

| File | Use |
|---|---|
| `logo.svg` | Icon on light backgrounds (dark binder rings) |
| `logo-dark.svg` | Icon on dark backgrounds (light binder rings) |
| `logo-wordmark.svg` | Icon and the name "CalCraft", for light backgrounds (README, docs) |
| `logo-wordmark-dark.svg` | The same for dark backgrounds |
| `favicon.svg` | Simplified glyph for 16–32 px: no rings, thicker band, larger spark |
| `favicon.ico` | 16, 32 and 48 px for clients that ask for `/favicon.ico` |
| `logo-16.png`, `logo-32.png` | The simplified glyph, transparent |
| `logo-48.png` … `logo-512.png` | The full icon (48, 180, 192, 512 px), transparent |
| `apple-touch-icon.png` | 180 px on an opaque white square (iOS fills transparency with black) |

The web UI uses `favicon.svg`, `favicon.ico`, `apple-touch-icon.png` and
`logo-192.png`/`logo-512.png` (as `icon-192.png`/`icon-512.png` in the web
app manifest); `scripts/build-ui.sh` copies them into the UI build, and the
server serves them same-origin under `/assets/` with content-hashed names.
The header and login page draw the mark as inline SVG so that its ring color
follows the light or dark theme.

## Colors

| Name | Hex | Role |
|---|---|---|
| Brand blue | `#2563EB` | Calendar page, primary buttons, links (light) |
| Deep blue | `#1D4ED8` | Header band of the page, hover |
| Ink | `#0F172A` | Binder rings and wordmark on light backgrounds |
| Light slate | `#E2E8F0` | Binder rings on dark backgrounds |
| White | `#FFFFFF` | The spark |

The UI's neutrals are the slate scale (`#F8FAFC` page background through
`#0F172A`); all tokens are CSS custom properties at the top of
`crates/calcraft-ui/style.css`.

## Usage

- Keep the spark white and the page blue; do not recolor the mark per theme
  except for the binder rings.
- Leave clear space of at least a quarter of the icon's width around it.
- Below 48 px use the simplified glyph (`favicon.svg`, `logo-16/32.png`).
- The name is written "CalCraft" (one word, two capitals).

## Sources and exports

The wordmark text is outlined to paths, so it renders identically everywhere
without a font; the letters come from Source Sans 3 Bold (SIL Open Font
License 1.1), with slightly tightened kerning. PNGs were rendered from the
SVGs with `rsvg-convert` and the ICO assembled with ImageMagick:

```sh
cd assets/brand
rsvg-convert -w 16 -h 16 favicon.svg -o logo-16.png
rsvg-convert -w 32 -h 32 favicon.svg -o logo-32.png
for s in 48 180 192 512; do rsvg-convert -w $s -h $s logo.svg -o logo-$s.png; done
rsvg-convert -w 144 -h 144 logo.svg -o /tmp/touch.png
magick -size 180x180 xc:white /tmp/touch.png -gravity center -composite -strip PNG24:apple-touch-icon.png
magick logo-16.png logo-32.png logo-48.png favicon.ico
```
