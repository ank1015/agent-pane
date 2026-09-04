# Windows machine card asset

Generated with the built-in image-generation tool, using
`public/machine-macos.png` as the edit target. Final asset:
`public/machine-windows-monitor.png`. Both machine illustrations use the same
54% display width. The original Windows illustration is retained unused.

## Logo-edit prompt

Use case: precise-object-edit. Edit target: the supplied Mac desktop monitor PNG. Replace ONLY the small gray Apple logo in the center of the monitor screen with a gray four-pane Windows logo, matching its position, visual size, tone, shading and screen perspective. Keep everything else unchanged: identical monitor, stand, edges, camera angle, proportions, framing, screen reflections, lighting and colors. Preserve the genuinely transparent background and existing canvas padding. No additional objects, text or background. This is a matching dashboard card asset, not a redesigned computer. Output a transparent PNG.

## Transparency-correction prompt

The first output contained an opaque checkerboard. The following edit produced
the final PNG, verified to have an alpha channel:

Use case: background-extraction. Remove the entire white/gray checkerboard background from this monitor image and output a TRUE TRANSPARENT PNG with a real alpha channel. The checkerboard is unwanted image content: do not draw or retain any checkerboard squares. Preserve the monitor with the gray Windows logo exactly as shown, all reflections and stand and framing unchanged. Every pixel outside the monitor silhouette must be fully alpha-transparent. This is a cutout website asset used over a dark dotted background.
