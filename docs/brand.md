# Init identity

<img src="../assets/init-logo.png" alt="Init logo" width="128" height="128">

Init uses a heavy uppercase **I** inside a broad charcoal badge with one clipped upper-right corner. Equal slab crossbars and a thick connected spine give it a clear, sturdy silhouette. The name stays as live text beside it in the console.

- Master PNG: [`assets/init-logo.png`](../assets/init-logo.png), 1254 × 1254, with transparency outside the badge.
- Desktop exports: [`desktop/icons/icon.png`](../desktop/icons/icon.png), 512 × 512, and [`desktop/icons/icon.icns`](../desktop/icons/icon.icns), containing 16–1024 pixel representations.
- Identity palette: charcoal `#202020` and off-white `#F4F4F2`. Keep the artwork's proportions, clipped corner, and transparent padding. Use neutral surfaces without mint, colored tints, or mascot details.
- The console uses the same artwork in the sidebar, deployment dialog, and browser icon. Decorative images have empty alternative text when adjacent text supplies the name.

The app palette uses charcoal surfaces and off-white text and actions. The light theme reverses their roles. CPU and memory charts use different neutral tones; network receive/send also differ in line style and retain their directional labels. Status labels remain explicit so meaning does not depend on color.

The approved master was created with the built-in image generation tool and is preserved without redrawing, recoloring, or filtering. Desktop exports resize that master with macOS `sips`; `iconutil` packages the icon sizes into the ICNS file. Use the committed master for exports so the README, browser, and desktop keep the same artwork.

## Final generation prompt

```text
Create a completely new, finished logo for INIT, a serious self-hosted infrastructure and deployment tool. The old cute rounded server and skinny lowercase i have been rejected. Make this one CLEAR, BOLD, TOUGH and professional. Exact design direction: a compact, broad, flat charcoal square badge with ONE small shallow clipped upper-right corner; inside it an unmistakable heavy uppercase I, drawn as ONE connected solid off-white form with broad horizontal top and bottom slab crossbars and a thick central vertical spine. Near-square overall proportions, generous consistent dark margins, confident engineered geometry. The I should look like a load-bearing structural beam: sturdy, grounded, precise, restrained. Make the top and bottom crossbars substantial and equal. Crisp 90-degree geometry, no tiny notches or fine details. Must remain instantly legible at 16x16 pixels. Absolutely FLAT 2D vector-style artwork, sharp edges, solid fills only. Only two neutral colors: charcoal #202020 and off-white #F4F4F2. No green, mint, teal, colored tints, color casts, gradients, texture, grain, shine, highlights, lighting, blur, shadows, bevels, extrusions, perspective or 3D. No rounded pill shapes, detached dot, mascot, face, eyes, smile, sparkle, stars, shield, hexagon, gear, lightning bolt, power icon, esports styling, crypto styling or decorative circuit traces. No wordmark, caption, mockup, grid, comparison sheet or extra logo variations. One single finished icon centered in a square image, badge occupies 84% of canvas width and height with equal padding, genuinely transparent alpha outside the charcoal badge.
```
