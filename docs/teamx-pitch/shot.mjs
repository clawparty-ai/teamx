// Screenshot the teamx FDE pitch pages into PNGs.
// Usage: node shot.mjs [outdir]
import { createRequire } from "module";
import { fileURLToPath } from "url";
import path from "path";
import fs from "fs";

const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(path.join(here, "..", "..", "tests", "ui-e2e", "package.json"));
const { chromium } = require("playwright");
const outdir = process.argv[2] || path.join(here, "out");
fs.mkdirSync(outdir, { recursive: true });

const htmlPath = "file://" + path.join(here, "pitch.html");
const pages = [
  { sel: "#p1", name: "01-overview" },
  { sel: "#p2", name: "02-why" },
  { sel: "#p3", name: "03-internal" },
  { sel: "#p4", name: "04-customer" },
];

const browser = await chromium.launch();
const ctx = await browser.newContext({ deviceScaleFactor: 2 });
const page = await ctx.newPage();
await page.goto(htmlPath, { waitUntil: "networkidle" });
for (const { sel, name } of pages) {
  const el = page.locator(sel);
  const box = await el.boundingBox();
  await el.screenshot({ path: path.join(outdir, `${name}.png`) });
  console.log(`saved ${name}.png`, box ? `(${Math.round(box.width)}x${Math.round(box.height)})` : "");
}
await browser.close();
console.log("done ->", outdir);
