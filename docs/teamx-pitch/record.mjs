// Record ~30s teamx admin-console walkthrough from the static prototype page.
// Uses prototype/dashboard.html (self-contained mock data, no backend needed).
// Output: video/teamx-admin-demo.mp4 (~30s) + srt optional.
import { createRequire } from "module";
import path from "path";
import { spawn } from "child_process";
import fs from "fs";
import http from "http";

const here = path.dirname(new URL(import.meta.url).pathname);
const root = path.join(here, "..", "..");
const protoDir = path.join(root, "docs", "prototype");
const outDir = path.join(here, "video");
fs.mkdirSync(outDir, { recursive: true });

const require = createRequire(path.join(root, "tests", "ui-e2e", "package.json"));
const { chromium } = require("playwright");

// ---- tiny static server for the prototype dir ----
const mime = { ".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".png": "image/png", ".svg": "image/svg+xml", ".json": "application/json" };
const server = http.createServer((req, res) => {
  let p = decodeURIComponent(req.url.split("?")[0]);
  if (p.endsWith("/")) p += "dashboard.html";
  const fp = path.join(protoDir, p);
  if (!fp.startsWith(protoDir) || !fs.existsSync(fp) || fs.statSync(fp).isDirectory()) { res.writeHead(404); res.end("404"); return; }
  res.writeHead(200, { "Content-Type": mime[path.extname(fp)] || "application/octet-stream" });
  fs.createReadStream(fp).pipe(res);
});
await new Promise((r) => server.listen(0, r));
const port = server.address().port;
const base = `http://127.0.0.1:${port}/dashboard.html`;

const browser = await chromium.launch();
const ctx = await browser.newContext({
  viewport: { width: 1440, height: 900 },
  recordVideo: { dir: outDir, size: { width: 1440, height: 900 } },
});
const page = await ctx.newPage();
await page.goto(base, { waitUntil: "networkidle" });

// helper
const wait = (ms) => page.waitForTimeout(ms);
async function go(view) {
  await page.click(`button[data-view="${view}"]`);
  await wait(650);
}

// --- walkthrough ---
await wait(700);                       // dashboard load
await wait(2600);                      // admire map

// topology toggle
await page.click('button[data-dashmode="topo"]');
await wait(2600);
await page.click('button[data-dashmode="map"]');
await wait(1400);

await go("kanban");
await wait(3000);

await go("cost");
await wait(3000);

await go("timeline");
await wait(3000);

await go("members");
await wait(2600);

await go("dashboard");
await wait(2600);

// back to a nice ending frame
await page.click('button[data-dashmode="topo"]');
await wait(1800);

// stop recording
await page.close();
await ctx.close();
await browser.close();
server.close();

// find latest webm
const webm = fs.readdirSync(outDir).find((f) => f.endsWith(".webm"));
if (!webm) { console.error("no webm produced"); process.exit(1); }
const src = path.join(outDir, webm);
const dst = path.join(outDir, "teamx-admin-demo.mp4");
console.log("webm ->", src);
await new Promise((resolve, reject) => {
  const ff = spawn("ffmpeg", ["-y", "-i", src, "-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "20", dst]);
  ff.stderr.on("data", () => {});
  ff.on("close", (c) => (c === 0 ? resolve() : reject(new Error("ffmpeg exit " + c))));
});
console.log("saved mp4 ->", dst);
