/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The compiled client (assets built with `--addons web,reporting`) opening the dynamic report screen from a menu, against explicit HTTP
// fixtures of the four report calls and the export route. Not backend/auth/database acceptance: the server side is covered by the reporting tests.
// Run through web/scripts/browser-reporting.sh (build, then this script); CHROME_BIN names a Chrome, CLIENT_BROWSER_OUTPUT an empty directory.
import assert from "node:assert/strict";
import { readFile, mkdir, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { hostname } from "node:os";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createRequire } from "node:module";
import { auditPage } from "../testing/axe.ts";
const qaRequire = createRequire(path.join(process.env.HOME, ".local/share/erpforall-qa/package.json"));
const { chromium } = qaRequire("playwright");
const root = fileURLToPath(new URL("../../", import.meta.url));
const output = process.env.CLIENT_BROWSER_OUTPUT, chromePath = process.env.CHROME_BIN;
if (!output || !chromePath) throw new Error("Set CLIENT_BROWSER_OUTPUT to an empty directory and CHROME_BIN to a Chrome executable.");
await mkdir(output, { recursive: true });
const manifest = JSON.parse(await readFile(path.join(root, "web/dist/human-ui/manifest.json"), "utf8"));
const assets = new Map();
for (const asset of manifest.assets) {
  const bytes = await readFile(path.join(root, "web/dist/human-ui", asset.path));
  assert.equal(createHash("sha256").update(bytes).digest("hex"), asset.sha256);
  assets.set("/erp/assets/" + asset.path, { data: bytes, type: asset.content_type });
}
for (const name of ["logo.svg", "logo-dark.svg"]) assets.set("/erp/brand/" + name, { data: await readFile(path.join(root, "crates/erp-server/brand/erpforall-" + name)), type: "image/svg+xml" });
const js = manifest.assets.find(asset => asset.name === "client.js"), css = manifest.assets.find(asset => asset.name === "client.css");
const bootstrap = { wire_major: 1, registry_version: "r1", asset_version: "fixture-a1", identity: { uid: 2, name: "Fixture user", internal: true }, companies: [{ id: 1, name: "Fixture company" }], current_company_id: 1, allowed_company_ids: [1], active_company_ids: [1], locale: { lang: "en_US", tz: "UTC", date_format: "%m/%d/%Y", time_format: "%H:%M:%S" }, csrf: { logout: "fixture-not-secret" } };

// ---- the report fixture: one section "Contacts" over countries, three per page, five in all ----
const REPORT = 7, rootId = `root~reporting.report~${REPORT}`, section = `${rootId}|line:PARTNERS~~`;
const countries = ["Belgium", "France", "Germany", "Italy", "Spain"];
const cell = (value, kind = "integer") => ({ kind, value, scale: 0, currency: null, zero: value === "0" });
const country = (index) => ({ id: `${section}|group:country_id~res.country~${index + 1}`, parent: section, name: countries[index], level: 1, kind: "detail", unfoldable: false, unfolded: false, cells: [cell(String(10 * (index + 1)))], caret: ["open"], remaining: null, next_offset: null });
const more = (offset) => ({ id: `${section}|more~~`, parent: section, name: "More", level: 1, kind: "more", unfoldable: false, unfolded: false, cells: [cell(null, "empty")], caret: [], remaining: countries.length - offset, next_offset: offset });
const baseOptions = { report: REPORT, companies: [1], date: { mode: "range", preset: "this_year", from: "2026-01-01", to: "2026-12-31" }, comparison: { mode: "none", periods: 1, order: "descending", from: null, to: null }, unfold_all: false, unfolded: [], loaded_pages: {}, hide_zero: false, search: "", provider: {},
  column_groups: [{ from: "2026-01-01", to: "2026-12-31" }], columns: [{ group: 0, name: "Contacts", label: "count", kind: "integer", sortable: true }], order: null };
const filters = { date_mode: "range", comparison: true, multi_company: false, unfold_all: true, hide_zero: "optional", search_bar: true, totals_below: false };
const topLine = (open) => ({ id: section, parent: null, name: "Contacts", level: 0, kind: "section", unfoldable: true, unfolded: open, cells: [cell("150")], caret: [], remaining: null, next_offset: null });
// Distinct fixture payloads verify transport fidelity. The export integration suite checks document structure.
const files = {
  pdf: { mime: "application/pdf", bytes: Buffer.from("%PDF-1.4 fixture") },
  xlsx: { mime: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", bytes: Buffer.from([0x50, 0x4b, 3, 4, 0x78]) }
};
const calls = [], errors = [], unknown = [], checks = [], screenshots = [];
let serverError;
let showPartnerLedger = false;
const ledgerOptions = { ...baseOptions, provider: { unreconciled: false },
  columns: ["Opening", "Debit", "Credit", "Closing"].map(name => ({ group: 0, name, label: name.toLowerCase(), kind: "monetary", sortable: false })) };
const ledgerLines = [
  { code: "RECEIVABLE", name: "Receivables", amounts: ["100", "50", "30", "120"] },
  { code: "PAYABLE", name: "Payables", amounts: ["-80", "10", "20", "-90"] },
].map(({ code, name, amounts }) => ({ ...topLine(false), id: rootId + "|line:" + code + "~~", name,
  cells: amounts.map(value => ({ ...cell(value, "monetary"), currency: "USD", scale: 2 })) }));
const server = createServer(async (req, res) => {
  try {
    const pathname = new URL(req.url, "http://fixture").pathname, asset = assets.get(pathname);
    if (asset) { res.setHeader("Content-Type", asset.type); res.end(asset.data); return; }
    if (req.method === "GET" && ["/erp", "/erp/action-30"].includes(pathname)) {
      res.setHeader("Content-Type", "text/html"); res.end(`<!doctype html><html lang="en"><meta name="viewport" content="width=device-width,initial-scale=1"><link rel="stylesheet" href="/erp/assets/${css.path}"><title>Report fixture</title><div id="erp-root"></div><script type="application/json" id="erp-bootstrap">${JSON.stringify(bootstrap)}</script><script type="module" src="/erp/assets/${js.path}"></script></html>`); return;
    }
    if (pathname === "/favicon.ico") { res.writeHead(204); res.end(); return; }
    let input = ""; for await (const chunk of req) { input += chunk; if (input.length > 65536) throw new Error("Oversized fixture request"); }
    const body = input ? JSON.parse(input) : {};
    calls.push({ path: pathname, body });
    if (pathname === "/erp/reporting/export") {
      assert.ok(Object.hasOwn(files, body.format));
      const file = files[body.format];
      res.writeHead(200, { "Content-Type": file.mime, "Content-Disposition": `attachment; filename="contacts.${body.format}"` });
      res.end(file.bytes); return;
    }
    const reportName = showPartnerLedger ? "Partner Ledger" : "Contacts by Country";
    let value;
    if (pathname.endsWith("erp_describe")) value = { wire_major: 1, registry_version: "r1", scope: body.scope, declarations: [] };
    else if (pathname.endsWith("erp_read_values")) value = { wire_major: 1, scope: body.scope, revision: "1", values: [] };
    else if (pathname.endsWith("res.users/read")) value = [{ id: 2, color_scheme: "light" }];
    else if (pathname.endsWith("res.users.settings/get_res_users_settings")) value = { homemenu_config: null };
    else if (pathname.endsWith("mail.activity/erp_activity_groups")) value = { activity_counter: 0, groups: [] };
    else if (pathname.endsWith("erp_load_web_menus")) value = { wire_major: 1, registry_version: "r1", roots: [10], nodes: [{ module: null, id: 10, parent_id: null, label: reportName, action_id: 30, action_kind: "client", children: [], icon: null }] };
    else if (pathname.endsWith("ir.actions.client/erp_load_web_client_action")) value = { wire_major: 1, result: { kind: "client", id: 30, name: reportName, tag: "reporting.report", params: { report: REPORT } } };
    else if (pathname.endsWith("reporting.report/erp_report_options")) value = {
      name: reportName, options: showPartnerLedger ? body.previous || ledgerOptions : baseOptions,
      filters: showPartnerLedger ? { ...filters, provider: "account_reporting.partner_ledger", unfold_all: false, hide_zero: "never" } : filters,
    };
    else if (pathname.endsWith("reporting.report/erp_report_lines")) {
      const open = body.options.unfolded.includes(section);
      const children = body.options.loaded_pages?.[section] ? countries.map((_, i) => country(i)) : [0, 1, 2].map(country).concat(more(3));
      value = { options: body.options, lines: showPartnerLedger ? ledgerLines : open ? [topLine(true), ...children] : [topLine(false)], computed_at: "2026-11-19 10:00:00", hidden_lines: [] };
    } else if (pathname.endsWith("reporting.report/erp_report_expand")) {
      value = { lines: body.offset === 0 ? [0, 1, 2].map(country).concat([more(3)]) : [3, 4].map(country), computed_at: "2026-11-19 10:00:01" };
    } else { unknown.push(pathname); throw new Error(`Unknown fixture RPC: ${pathname}`); }
    res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(value));
  } catch (error) { serverError = error; res.writeHead(500, { "Content-Type": "application/json" }); res.end(JSON.stringify({ message: "Fixture server failure" })); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch({ executablePath: chromePath, headless: true });
const result = { status: "FAIL", scope: "compiled client assets with explicit HTTP fixtures; not backend/auth/database acceptance", host: hostname(), time_utc: new Date().toISOString(), node: process.version, browser: browser.version(), assets: manifest.assets, checks, screenshots };
let activePage;
const rowNames = page => page.locator("tr.report-line .report-label").allTextContents();
try {
  const page = await browser.newPage({ acceptDownloads: true }); activePage = page;
  page.on("pageerror", error => errors.push(error.message));
  page.on("console", message => { if (message.type() === "error") errors.push(message.text()); });
  for (const width of [1440, 820, 390]) {
    await page.setViewportSize({ width, height: 900 });
    calls.length = 0;
    await page.goto(origin + "/erp/action-30?menu=10");
    await page.locator("table.report-table").waitFor();
    assert.deepEqual(await rowNames(page), ["Contacts"]);
    assert.equal(await page.locator("tr.report-line .report-cell").first().textContent(), "150");
    assert.deepEqual(calls.filter(c => c.path.includes("reporting.report/")).map(c => c.path.split("/").pop()), ["erp_report_options", "erp_report_lines"]);
    const overflow = await page.evaluate(() => document.scrollingElement.scrollWidth - window.innerWidth);
    assert.ok(overflow <= 1, `the page scrolls sideways by ${overflow}px at ${width}`);
    await page.screenshot({ path: path.join(output, `report-folded-${width}.png`), fullPage: true }); screenshots.push(`report-folded-${width}.png`);

    // unfold by click, load more, fold
    await page.getByRole("button", { name: "Unfold Contacts" }).click();
    await page.locator("tr.report-line", { hasText: "Belgium" }).waitFor();
    assert.deepEqual(await rowNames(page), ["Contacts", "Belgium", "France", "Germany"]);
    assert.equal(await page.locator("tr.report-line[aria-level='2']").count(), 4, "three countries and the load-more row sit one level below the section");
    await page.getByRole("button", { name: "Show more (2 remaining)" }).click();
    await page.locator("tr.report-line", { hasText: "Spain" }).waitFor();
    assert.deepEqual(await rowNames(page), ["Contacts", "Belgium", "France", "Germany", "Italy", "Spain"]);
    assert.deepEqual(calls.findLast(c => c.path.endsWith("erp_report_lines")).body.options.loaded_pages, { [section]: 1 });
    const geometry = await page.evaluate(() => ({ labels: [...document.querySelectorAll("tr.report-line .report-label")].map(node => Math.round(node.getBoundingClientRect().left)), align: getComputedStyle(document.querySelector("tr.report-line .report-cell")).textAlign }));
    assert.ok(geometry.labels[1] - geometry.labels[0] >= 16, `a child line is indented below its section: ${JSON.stringify(geometry.labels)}`);
    assert.ok(["end", "right"].includes(geometry.align), `numbers are aligned to the end of the cell, not ${geometry.align}`);
    await page.screenshot({ path: path.join(output, `report-unfolded-${width}.png`), fullPage: true }); screenshots.push(`report-unfolded-${width}.png`);
    await page.getByRole("button", { name: "Fold Contacts" }).click();
    assert.deepEqual(await rowNames(page), ["Contacts"]);

    // keyboard: Right unfolds the focused line, Down moves, Left goes back to the parent, Left folds
    await page.locator("tr.report-line").first().focus();
    await page.keyboard.press("ArrowRight");
    await page.locator("tr.report-line", { hasText: "Belgium" }).waitFor();
    await page.keyboard.press("ArrowDown");
    assert.equal(await page.evaluate(() => document.activeElement?.querySelector(".report-label")?.textContent), "Belgium");
    await page.keyboard.press("ArrowLeft");
    assert.equal(await page.evaluate(() => document.activeElement?.querySelector(".report-label")?.textContent), "Contacts");
    await page.keyboard.press("ArrowLeft");
    assert.deepEqual(await rowNames(page), ["Contacts"]);

    // sort, preset, search send options and the answer is shown
    await page.getByRole("button", { name: "Contacts", exact: true }).click();
    await page.waitForFunction(() => document.querySelector(".report-col")?.getAttribute("aria-sort") === "descending");
    assert.deepEqual(calls.findLast(c => c.path.endsWith("erp_report_lines")).body.options.order, { column: "count", direction: "desc" });
    await page.locator("[data-filter=date] .report-filter-button").click();
    await page.locator("select[name=preset]").selectOption("last_year");
    await page.waitForFunction(() => document.querySelector(".report-status")?.textContent?.includes("Computed"));
    assert.equal(calls.findLast(c => c.path.endsWith("erp_report_lines")).body.options.date.preset, "last_year");
    await page.getByRole("button", { name: "Unfold Contacts" }).click();
    await page.locator("tr.report-line", { hasText: "France" }).waitFor();
    await page.getByRole("button", { name: "Show more (2 remaining)" }).click();
    await page.locator("tr.report-line", { hasText: "Spain" }).waitFor();
    await page.getByRole("searchbox", { name: "Search lines" }).fill("spa");
    await page.keyboard.press("Enter");
    await page.waitForFunction(() => document.querySelector(".report-status")?.textContent?.includes("Computed"));
    assert.equal(calls.findLast(c => c.path.endsWith("erp_report_lines")).body.options.search, "");

    // exports: the file arrives as sent and the request carries options and a format, never rows
    await page.locator("tr.report-line", { hasText: "France" }).waitFor({ state: "detached" });
    assert.deepEqual(await rowNames(page), ["Contacts", "Spain"]);
    for (const [format, file] of Object.entries(files)) {
      const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: format.toUpperCase(), exact: true }).click()]);
      assert.equal(download.suggestedFilename(), "contacts." + format);
      assert.deepEqual(await readFile(await download.path()), file.bytes);
      const exported = calls.findLast(c => c.path === "/erp/reporting/export").body;
      assert.deepEqual(Object.keys(exported).sort(), ["format", "options", "report"]);
      assert.equal(exported.format, format);
      assert.equal(exported.options.search, "spa");
      assert.deepEqual(exported.options.loaded_pages, { [section]: 1 });
    }
    if (width === 1440) { await auditPage(page); await page.screenshot({ path: path.join(output, "report-audit-1440.png"), fullPage: true }); screenshots.push("report-audit-1440.png"); }
    checks.push({ width, open_from_menu: "PASS", unfold_load_more_fold: "PASS", keyboard: "PASS", sort_preset_search: "PASS", export: "PASS", no_page_overflow: "PASS", axe_serious: width === 1440 ? "PASS" : "not run at this width" });
  }
  // A provider with historical open items can offer its checkbox without other Options controls.
  showPartnerLedger = true;
  await page.setViewportSize({ width: 820, height: 900 });
  await page.goto(origin + "/erp/action-30?menu=10");
  await page.locator("tr.report-line", { hasText: "Receivables" }).waitFor();
  assert.deepEqual(await rowNames(page), ["Receivables", "Payables"]);
  for (const enabled of [true, false]) {
    await page.getByRole("button", { name: "Options", exact: true }).click();
    const box = page.getByRole("checkbox", { name: "Unreconciled at period end", exact: true });
    assert.equal(await box.isChecked(), !enabled);
    if (!enabled) {
      const shot = "report-open-items-820.png";
      await page.screenshot({ path: path.join(output, shot), fullPage: true });
      screenshots.push(shot);
    }
    const [response] = await Promise.all([
      page.waitForResponse(response => new URL(response.url()).pathname.endsWith("/erp_report_lines")),
      box.setChecked(enabled),
    ]);
    assert.equal(response.request().postDataJSON().options.provider.unreconciled, enabled);
    await page.locator('.report-scroll[data-dim="false"]').waitFor();
  }
  checks.push({ width: 820, historical_open_items_selection: "PASS" });
  if (serverError) throw serverError;
  assert.deepEqual(unknown, []);
  assert.deepEqual(errors, []); result.status = "PASS";
} catch (error) { result.error = error instanceof Error ? error.stack : String(error); if (activePage) await activePage.screenshot({ path: path.join(output, "failure.png"), fullPage: true }); process.exitCode = 1; }
finally { result.unknown_fixture_calls = unknown; await writeFile(path.join(output, "reporting.json"), JSON.stringify(result, null, 2) + "\n"); await browser.close(); server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); }
