// Convert assets/discord/*.svg → 1024×1024 PNGs ready to upload to
// Discord's Rich Presence Art Assets page. Run with `bun scripts/
// build-discord-assets.mjs` after editing any of the SVG sources.
//
// Discord's art assets endpoint accepts only PNG / JPG / JPEG and
// recommends 1024×1024. SVGs are rejected outright.

import { mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const srcDir = join(root, "assets", "discord");
const outDir = join(root, "assets", "discord", "png");

mkdirSync(outDir, { recursive: true });

const assets = ["waveflow_logo", "play", "pause"];

for (const name of assets) {
  const src = join(srcDir, `${name}.svg`);
  const out = join(outDir, `${name}.png`);
  await sharp(src, { density: 384 })
    .resize(1024, 1024, {
      fit: "contain",
      background: { r: 0, g: 0, b: 0, alpha: 0 },
    })
    .png({ compressionLevel: 9 })
    .toFile(out);
  console.log(`wrote ${out}`);
}
