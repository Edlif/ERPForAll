/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Real native server and compiled addon assets; fixture cookies arrive only through stdin.
import assert from "node:assert/strict";
import {mkdir, writeFile} from "node:fs/promises";
import {hostname} from "node:os";
import path from "node:path";
import {chromium} from "playwright";
let input = ""; for await (const chunk of process.stdin) input += chunk;
const {origin, cookies, ids} = JSON.parse(input);
const output = process.env.AI_BROWSER_OUTPUT;
if (!output) throw new Error("AI_BROWSER_OUTPUT is required");
await mkdir(output, {recursive: true});
const browser = await chromium.launch({executablePath: process.env.CHROME_BIN ?? (process.platform === "darwin" ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "/usr/bin/google-chrome"), headless: true});
const failures = [], external = [], checks = [], shots = [];
const context = await browser.newContext({viewport: {width: 1440, height: 900}});
await context.addCookies(cookies.split("; ").map(cookie => { const i = cookie.indexOf("="); return {name: cookie.slice(0, i), value: cookie.slice(i + 1), url: origin}; }));
const page = await context.newPage();
page.on("pageerror", error => failures.push(error.message));
page.on("request", request => { if (new URL(request.url()).origin !== origin) external.push(new URL(request.url()).origin); });
page.on("response", response => { if (response.status() >= 400) failures.push(`${response.status()} ${new URL(response.url()).pathname}`); });
const settings = `${origin}/erp/action-${ids["base.settings_action"]}?menu=${ids["base.settings_menu"]}`;
async function open(section) {
  await page.goto(settings, {waitUntil: "networkidle"});
  const links = {provider: ["providers", "Providers"], route: ["routes", "Company routes"], budget: ["budgets", "Budgets"], usage: ["usage", "Usage"]};
  const [key, name] = links[section];
  await page.locator(`[data-setting-key="ai_base.${key}"]`).getByRole("button", {name, exact: true}).click();
  await page.waitForURL(url => url.pathname === `/erp/action-${ids[`ai_base.${section}_action`]}` && url.searchParams.get("menu") === String(ids[`ai_base.${section}_menu`]));
  await page.locator('table[aria-label="Records"]').waitFor();
}
async function models() { const control = page.locator('textarea[name="allowed_models"]'); await control.waitFor(); return control; }
try {
  await open("provider");
  const providerRow = page.getByRole("row").filter({has: page.getByRole("cell", {name: "local", exact: true})});
  assert.equal(await providerRow.count(), 1, "the seeded provider has one row");
  await providerRow.getByRole("cell", {name: "local", exact: true}).first().click();
  let control = await models();
  assert.equal(await control.inputValue(), "local-model");
  await control.fill("local-model\nprovider/browser");
  const saved = page.waitForResponse(response => response.url().endsWith("/ai.provider/web_save") && response.status() === 200);
  await control.press("Alt+s"); await saved;
  await page.reload({waitUntil: "networkidle"}); control = await models();
  assert.equal(await control.inputValue(), "local-model\nprovider/browser");
  checks.push("unblurred_keyboard_save_round_trip");
  await control.fill("two words");
  assert.equal(await control.getAttribute("aria-invalid"), "true");
  await control.press("Alt+j");
  await page.waitForFunction(() => document.querySelector('textarea[name="allowed_models"]')?.value === "local-model\nprovider/browser");
  checks.push("keyboard_discard_invalid_draft");
  control = await models(); await control.fill("two words");
  // Back uses the client's guarded history path, rather than a full document navigation.
  await page.goBack();
  const dialog = page.getByRole("dialog", {name: "Unsaved changes"});
  await dialog.waitFor(); await dialog.getByRole("button", {name: "Cancel", exact: true}).click();
  assert.equal(await (await models()).inputValue(), "two words");
  await (await models()).press("Alt+j");
  checks.push("invalid_only_navigation_guard");
  await open("budget");
  await page.getByRole("cell", {name:"Retail assistant", exact:true}).click();
  const limit = page.locator('input[name="cost_limit"]');
  await limit.waitFor();
  assert.equal(await limit.inputValue(), "5000000000");
  await limit.fill("5000000001");
  const budgetSaved = page.waitForResponse(response => response.url().endsWith("/ai.budget/web_save") && response.ok());
  await limit.press("Tab");
  await page.getByRole("button",{name:"Save",exact:true}).click();
  await budgetSaved;
  await page.reload({waitUntil:"networkidle"});
  assert.equal(await limit.inputValue(),"5000000001");
  await page.screenshot({path:path.join(output,"budget-desktop.png")});
  await open("usage");
  assert.equal(await page.getByRole("button",{name:"New",exact:true}).count(),0);
  checks.push("company_budget_exact_cost_round_trip_and_read_only_usage");
  for (const theme of ["light", "dark"]) {
    await page.emulateMedia({colorScheme: theme});
    for (const width of [1440, 800, 390]) {
      await page.setViewportSize({width, height: 900});
      for (const section of ["provider", "route"]) {
        await open(section);
        await page.getByRole("button", {name: "New", exact: true}).click();
        await page.locator('input[name="name"]').waitFor();
        if (section === "provider") await models();
        else await page.locator('input[name="model"]').waitFor();
        await page.waitForLoadState("networkidle");
        assert.equal(await page.getByRole("dialog").count(), 0, "New must not open an error dialog");
        const geometry = await page.evaluate(() => ({viewport: innerWidth, document: document.documentElement.scrollWidth}));
        assert.ok(geometry.document <= geometry.viewport + 1, `form overflow: ${JSON.stringify(geometry)}`);
        const file = `${section}-${theme}-${width}.png`;
        await page.screenshot({path: path.join(output, file), animations: "disabled"});
        shots.push({file, theme, width, geometry});
      }
    }
  }
  checks.push("provider_route_new_desktop_phone_light_dark");
  assert.deepEqual(external, [], "configuration must not call a provider");
  assert.deepEqual(failures, [], "browser or HTTP failures");
  const result = {host: hostname(), utc: new Date().toISOString(), node: process.version, browser: await browser.version(), checks, shots, external, failures};
  await writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2) + "\n");
  console.log(JSON.stringify(result));
} catch (error) {
  await page.screenshot({path: path.join(output, "failure.png")});
  await writeFile(path.join(output, "failure.txt"), `${error.stack}\n${JSON.stringify({checks, failures, external})}\n`);
  throw error;
} finally { await browser.close(); }
