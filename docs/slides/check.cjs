// overflow check: verify each slide fits its 1280x720 box
const { chromium } = require('@playwright/test');
const path = require('node:path');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  await page.goto('file://' + path.join(__dirname, 'product-dev-team-slides.html'), { waitUntil: 'networkidle' });
  await page.evaluate(() => document.fonts?.ready);

  const n = await page.locator('.slide').count();
  for (let i = 0; i < n; i++) {
    const res = await page.locator('.slide').nth(i).evaluate((el) => {
      const r = el.getBoundingClientRect();
      // any descendant overflowing the slide box?
      const overflow = [];
      el.querySelectorAll('*').forEach((c) => {
        const cr = c.getBoundingClientRect();
        if (cr.width === 0 && cr.height === 0) return;
        const rel = { left: cr.left - r.left, right: cr.right - r.left, top: cr.top - r.top, bottom: cr.bottom - r.top };
        if (rel.right > r.width + 1 || rel.left < -1 || rel.bottom > r.height + 1 || rel.top < -1) {
          overflow.push({ cls: (c.className && c.className.baseVal !== undefined ? c.className.baseVal : c.className) || c.tagName, right: Math.round(rel.right), bottom: Math.round(rel.bottom), left: Math.round(rel.left), top: Math.round(rel.top) });
        }
      });
      return { w: r.width, h: r.height, scrollW: el.scrollWidth, scrollH: el.scrollHeight, overflow: overflow.slice(0, 6) };
    });
    const flag = res.scrollH > 722 || res.scrollW > 1282 || res.overflow.length > 0 ? '!! OVERFLOW' : 'ok';
    console.log(`slide-${String(i + 1).padStart(2, '0')} [${flag}] box=${Math.round(res.w)}x${Math.round(res.h)} scroll=${Math.round(res.scrollW)}x${Math.round(res.scrollH)}`);
    if (res.overflow.length) console.log('   ' + JSON.stringify(res.overflow));
  }
  await browser.close();
})().catch((e) => { console.error(e); process.exit(1); });
