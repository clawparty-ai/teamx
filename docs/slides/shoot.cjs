// teamx slides screenshot script (CommonJS, NODE_PATH-aware)
// Usage: NODE_PATH=<openclaw npm-global>/lib/node_modules node shoot.cjs
// Outputs: out/slide-NN.png (per-page) + out/slides-long.png (full deck)
const { chromium } = require('@playwright/test');
const path = require('node:path');
const fs = require('node:fs');

const outDir = path.join(__dirname, 'out');
fs.mkdirSync(outDir, { recursive: true });

const W = 1280, H = 720, SCALE = 2;

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: W, height: H }, deviceScaleFactor: SCALE });
  await page.goto('file://' + path.join(__dirname, 'product-dev-team-slides.html'), { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts?.ready);

  // 1) per-page screenshots
  const slides = await page.locator('.slide').count();
  const perPage = [];
  for (let i = 0; i < slides; i++) {
    const el = page.locator('.slide').nth(i);
    const file = path.join(outDir, `slide-${String(i + 1).padStart(2, '0')}.png`);
    await el.screenshot({ path: file });
    perPage.push({ file, w: W * SCALE, h: H * SCALE });
  }

  // 2) full-deck long image
  const long = path.join(outDir, 'slides-long.png');
  await page.screenshot({ path: long, fullPage: true });

  const box = await page.evaluate(() => ({
    longH: document.body.scrollHeight,
    viewport: document.documentElement.scrollWidth,
  }));
  console.log(JSON.stringify({ perPage, longImage: { file: long, w: box.viewport * SCALE, h: box.longH * SCALE }, ok: true }));

  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
