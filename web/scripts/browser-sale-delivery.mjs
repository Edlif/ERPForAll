/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Real installed server and compiled client. No routes are intercepted.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { hostname } from "node:os";
import path from "node:path";
import { chromium } from "playwright";

let input = "";
for await (const chunk of process.stdin) input += chunk;
const { origin, cookies, action, order, warehouse, warehouseName } = JSON.parse(input);
const output = process.env.SALE_DELIVERY_BROWSER_OUTPUT;
if (!output) throw new Error("SALE_DELIVERY_BROWSER_OUTPUT is required");
await mkdir(output, { recursive: true });
const executablePath = process.env.CHROME_BIN ?? (process.platform === "darwin"
  ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "/usr/bin/google-chrome");
const browser = await chromium.launch({ executablePath, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"] });
const result = { host: hostname(), utc: new Date().toISOString(), node: process.version,
  browser: browser.version(), checks: [], screenshots: [], status: "failed" };
let page;
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await context.addCookies(cookies.split("; ").map(cookie => {
    const separator = cookie.indexOf("=");
    assert.ok(separator > 0, "malformed fixture cookie");
    return { name: cookie.slice(0, separator), value: cookie.slice(separator + 1), url: origin };
  }));
  page = await context.newPage();
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  const route = `${origin}/erp/action-${action}?id=${order}&view_type=form`;
  const screenshot = async name => {
    const file = path.join(output, name);
    await page.screenshot({ path: file, fullPage: true });
    result.screenshots.push(file);
  };
  const readOrder = async () => {
    const reply = await page.evaluate(async id => {
      const response = await fetch("/erp/json/2/sale.order/read", {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ ids: [id], fields: ["state", "warehouse_id", "direct_delivery_id", "name"] }),
      });
      return { status: response.status, body: await response.json() };
    }, order);
    assert.equal(reply.status, 200, JSON.stringify(reply.body));
    return reply.body[0];
  };
  await page.goto(route, { waitUntil: "networkidle" });
  const picker = page.getByRole("combobox", { name: "Search Warehouse", exact: true });
  await picker.waitFor();
  await picker.focus();
  await picker.pressSequentially(warehouseName);
  await page.getByRole("option", { name: warehouseName, exact: true }).waitFor();
  await picker.press("ArrowDown");
  const changed = page.waitForResponse(response => response.url().endsWith("/sale.order/onchange") && response.request().method() === "POST");
  await picker.press("Enter");
  assert.equal((await changed).status(), 200, "warehouse onchange succeeds");
  await page.waitForFunction(() => {
    const input = document.querySelector('input[aria-label="Search Warehouse"]');
    return input instanceof HTMLInputElement && !input.disabled;
  });
  await page.getByRole("button", { name: "Save", exact: true }).waitFor();
  const saved = page.waitForResponse(response => response.url().endsWith("/sale.order/web_save") && response.request().method() === "POST");
  await page.keyboard.press("Alt+s");
  assert.equal((await saved).status(), 200, "keyboard save succeeds");
  const persisted = await readOrder();
  const selected = Array.isArray(persisted.warehouse_id) ? persisted.warehouse_id[0] : persisted.warehouse_id;
  assert.equal(selected, warehouse, "warehouse selection persisted through actual RPC");
  assert.equal(persisted.state, "draft");
  result.checks.push("keyboard warehouse selection and save persisted");
  await screenshot("warehouse-saved-desktop.png");
  const confirmed = page.waitForResponse(response => response.url().endsWith("/sale.order/action_confirm") && response.request().method() === "POST");
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  assert.equal((await confirmed).status(), 200, "actual confirmation succeeds");
  assert.equal((await readOrder()).state, "sale");
  await page.reload({ waitUntil: "networkidle" });
  const readonly = page.locator(".field-readonly").filter({ hasText: "Warehouse" });
  await readonly.getByText(warehouseName, { exact: true }).waitFor();
  assert.equal(await picker.count(), 0, "confirmed warehouse has no editor");
  result.checks.push("confirmed order displays persisted readonly warehouse after reload");
  await screenshot("warehouse-confirmed-desktop.png");
  await page.setViewportSize({ width: 390, height: 844 });
  await page.reload({ waitUntil: "networkidle" });
  await readonly.getByText(warehouseName, { exact: true }).waitFor();
  await readonly.scrollIntoViewIfNeeded();
  const box = await readonly.boundingBox();
  assert.ok(box && box.x >= 0 && box.x + box.width <= 391, "warehouse fits narrow viewport");
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false, "phone page has no horizontal overflow");
  assert.equal(await picker.count(), 0);
  await screenshot("warehouse-confirmed-phone.png");
  result.checks.push("readonly warehouse fits 390px layout without page overflow");
  await page.setViewportSize({ width: 1440, height: 900 });
  const deliveryLink = page.getByRole("button", { name: /^Open Delivery: / });
  const deliveryName = (await deliveryLink.innerText()).trim();
  assert.ok(deliveryName);
  const confirmedOrder = await readOrder();
  const transfer = confirmedOrder.direct_delivery_id;
  const transferId = Array.isArray(transfer) ? transfer[0] : transfer;
  assert.ok(Number.isSafeInteger(transferId) && transferId > 0);
  assert.equal(await deliveryLink.locator("svg.related-record-arrow").count(), 1);
  await deliveryLink.click();
  const sourceOrder = page.getByRole("button", { name: /^Open Sales Order: / });
  await sourceOrder.waitFor();
  assert.equal(new URL(page.url()).searchParams.get("id"), String(transferId));
  assert.equal((await sourceOrder.innerText()).trim(), confirmedOrder.name);
  assert.match(await page.locator("[data-control-breadcrumbs]").innerText(), /\bDeliveries\b/);
  await screenshot("delivery-source-order.png");
  await sourceOrder.click();
  await deliveryLink.waitFor();
  assert.equal(new URL(page.url()).searchParams.get("id"), String(order));
  assert.equal((await deliveryLink.innerText()).trim(), deliveryName);
  result.checks.push("Delivery opens the saved shipment and Sales Order returns to its original order");
  await deliveryLink.click();
  const validate = page.getByRole("button", { name: "Validate", exact: true });
  await validate.click();
  const decision = page.getByRole("dialog", { name: "Remaining demand" });
  await decision.waitFor();
  await decision.getByText("Browser Shipping Cartons", { exact: true }).waitFor();
  await decision.getByRole("button", { name: "Close Remaining Demand" }).waitFor();
  await screenshot("backorder-ask-desktop.png");
  await decision.getByRole("button", { name: "Cancel", exact: true }).click();
  await decision.waitFor({ state: "hidden" });
  await validate.click();
  await decision.waitFor();
  await page.setViewportSize({ width: 390, height: 844 });
  const bounds = await decision.boundingBox();
  assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= 391, "phone dialog fits the viewport");
  await screenshot("backorder-ask-phone.png");
  const completed = page.waitForResponse(response => response.url().endsWith("/stock.picking/action_keep_backorder"));
  await decision.getByRole("button", { name: "Create Backorder" }).click();
  assert.equal((await completed).status(), 200, "backorder action succeeds");
  await decision.waitFor({ state: "hidden" });
  await validate.waitFor({ state: "hidden" });
  result.checks.push("Validate opens the live Ask dialog, Cancel preserves the transfer, and Create Backorder finishes it");
  assert.deepEqual(errors, [], "no uncaught browser errors");
  result.status = "passed";
} catch (error) {
  result.error = error instanceof Error ? error.stack : String(error);
  if (page) {
    const file = path.join(output, "failure.png");
    try { await page.screenshot({ path: file, fullPage: true }); result.screenshots.push(file); }
    catch (captureError) { result.captureError = String(captureError); }
  }
  throw error;
} finally {
  await writeFile(path.join(output, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify(result));
  await browser.close();
}
