# Favicons

`favicon.svg` is the NetRoll logomark and the source of truth — it mirrors
`src/ui/components/Logomark.tsx` (concentric radio-signal arc in a cyan rounded
square), with the dark theme's token values hardcoded because a favicon cannot
read CSS custom properties and dark is the product default.

`favicon-16.svg` exists only to supply the **16px layer** of `favicon.ico`. At
that size the full mark's two concentric arcs land at 2.8px and 1.4px radii and
merge into a blob, so the small variant keeps one heavier arc over the dot.

`favicon.ico` is a generated 48/32/16 artifact for browsers without SVG-favicon
support. Regenerate it after any change to either SVG:

```sh
cd frontend/public
rsvg-convert -w 48 -h 48 favicon.svg    -o /tmp/i48.png
rsvg-convert -w 32 -h 32 favicon.svg    -o /tmp/i32.png
rsvg-convert -w 16 -h 16 favicon-16.svg -o /tmp/i16.png
magick /tmp/i48.png /tmp/i32.png /tmp/i16.png favicon.ico
```

`rsvg-convert` (librsvg) is required: ImageMagick's built-in SVG renderer does
not stroke paths at all and silently emits a bare cyan square.
