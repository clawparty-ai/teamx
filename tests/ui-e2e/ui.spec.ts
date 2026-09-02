import { test, expect } from "@playwright/test"
import { readFileSync } from "node:fs"
import { join } from "node:path"

const env = JSON.parse(readFileSync(join(__dirname, "env.json"), "utf8")) as { baseURL: string; token: string }
const base = env.baseURL
const tokenUrl = `${base}/?token=${env.token}`

// Block external resources (Leaflet CDN / map tiles) so the Dashboard falls
// back to the self-contained SVG renderer deterministically in tests.
async function blockExternal(context: import("@playwright/test").BrowserContext) {
  await context.route(/^https?:\/\/(?!127\.0\.0\.1)/, (route) => route.abort())
}

test("登录：token 设 cookie 并进入 Dashboard（默认视图）", async ({ page }) => {
  const ctx = page.context()
  await blockExternal(ctx)
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  await expect(page).toHaveURL(new RegExp(`^${base}/$`))
  await expect(page.locator("#view-dashboard")).toBeVisible()
  await expect(page.locator('nav button[data-view="dashboard"]')).toHaveClass(/active/)
  await expect(page.locator("#dashOnline")).toHaveText(/\/ 2/)
})

test("Dashboard 无外部网络时降级 SVG 渲染，节点可见", async ({ page }) => {
  const ctx = page.context()
  await blockExternal(ctx)
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  await expect(page.locator("#dashSvgFallback")).toBeVisible({ timeout: 15000 })
  const text = await page.locator("#dashSvgFallback").textContent()
  expect(text).toContain("bob")
  expect(text).toContain("UIe2e")
})

test("地图/拓扑模式切换", async ({ page }) => {
  const ctx = page.context()
  await blockExternal(ctx)
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  // topology mode: the SVG fallback renders the server-member ring
  await page.locator('button[data-dmode="topo"]').click()
  await expect(page.locator('button[data-dmode="topo"]')).toHaveClass(/active/)
  await expect(page.locator("#dashSvgFallback")).toBeVisible()
  // back to map mode
  await page.locator('button[data-dmode="map"]').click()
  await expect(page.locator('button[data-dmode="map"]')).toHaveClass(/active/)
  await expect(page.locator("#dashSvgFallback")).toBeVisible()
})

test("视图切换：KanBan / Goal / Cost / Timeline / Members", async ({ page }) => {
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  for (const v of ["kanban", "goal", "cost", "timeline", "members"]) {
    await page.locator(`nav button[data-view="${v}"]`).click()
    await expect(page.locator(`#view-${v}`)).toBeVisible()
    await expect(page.locator(`nav button[data-view="${v}"]`)).toHaveClass(/active/)
  }
})

test("主题切换持久化 .dark", async ({ page }) => {
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  await page.locator("#themeToggle").click()
  await expect(page.locator("html")).toHaveClass(/dark/)
  await page.locator("#themeToggle").click()
  await expect(page.locator("html")).not.toHaveClass(/dark/)
})

test("时间范围快速选择更新标签", async ({ page }) => {
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  await page.locator("#timeBtn").click()
  await page.locator('.tp-quick[data-from="now-24h"]').click()
  await expect(page.locator("#timeLabel")).toHaveText("Last 24 hours")
})

test("自适应：数据视图容器 full-width", async ({ page }) => {
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  const cls = await page.locator(".container").getAttribute("class")
  expect(cls).toContain("full-width")
})

test("响应式：窄屏仍可访问且视图可切换", async ({ page }) => {
  await page.setViewportSize({ width: 420, height: 800 })
  await page.goto(tokenUrl, { waitUntil: "networkidle" })
  await expect(page.locator("#view-dashboard")).toBeVisible()
  await page.locator('nav button[data-view="cost"]').click()
  await expect(page.locator("#view-cost")).toBeVisible()
})
