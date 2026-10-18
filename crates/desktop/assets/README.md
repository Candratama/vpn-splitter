# App icons

CloudServerIcon (Stroke Rounded) from Hugeicons:
https://github.com/hugeicons/hugeicons-static/blob/main/icons/cloud-server.svg

`cloud-server.svg` is the original asset. MIT attribution is included in
`HUGEICONS-LICENSE.md` and copied into the macOS bundle.

`app-icon.svg` uses the original paths in white on a dark rounded square.
`AppIcon.icns` contains 16, 32, 128, 256 and 512 point images at 1x and 2x.
`app-icon.png` is a 512-pixel preview.

`menubar-icon.svg` uses white strokes on a transparent background. Its PNG
and straight-alpha RGBA are 18 by 18 pixels. The menu bar shows only the icon,
without a text title. The tray embeds the RGBA directly, with no runtime SVG
renderer. This is a fixed image, not an AppKit template image.

Raster assets were rendered with resvg 0.46.0; the macOS iconset was assembled
with `iconutil -c icns`. Regenerate the raster assets when changing the SVGs.
