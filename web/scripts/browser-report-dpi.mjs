/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Real Settings component and native RPC/storage. Only the containing page is a fixture.
import assert from "node:assert/strict";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { createServer } from "node:http";
import { hostname } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";
import { chromium } from "playwright";

let input = "";
for await (const chunk of process.stdin) input += chunk;
const { origin, cookies } = JSON.parse(input);
assert.match(origin, /^http:\/\/127\.0\.0\.1:\d+$/);
const output = process.env.REPORT_DPI_OUTPUT;
assert.ok(output, "REPORT_DPI_OUTPUT is required");
await mkdir(output, { recursive: true });
const root = fileURLToPath(new URL("../../", import.meta.url));
const entry = `
import { settingsView } from ${JSON.stringify(path.join(root, "addons/web/static/src/client/services.ts"))};
import { RpcClient } from ${JSON.stringify(path.join(root, "addons/web/static/src/client/transport.ts"))};
const rpc = new RpcClient(fetch.bind(window), () => { throw new Error('unexpected session expiry'); });
await settingsView(document.querySelector('#host'), rpc, [],
  job => { job().catch(error => { document.querySelector('#error').textContent = error.message; }); },
  message => { document.querySelector('#notice').textContent = message; }, {}, ['global'],
  undefined, undefined, undefined, 'base',
  { navigation: { roots: [{id:1,module:'base',label:'General Settings',icon:'lucide:settings'}],
    openTarget: async () => { throw new Error('unexpected navigation'); } } });
window.fixtureReady = true;
`;
const bundle = await build({ stdin: { contents: entry, resolveDir: path.join(root, "web"), sourcefile: "report-dpi-fixture.ts", loader: "ts" }, bundle: true, write: false, format: "esm", platform: "browser", target: "es2023" });
const css = await readFile(path.join(root, "addons/web/static/src/client/client.css"));
const server = createServer(async (request, response) => {
  try {
    if (request.url.startsWith("/erp/json/2/")) {
      let body = "";
      for await (const chunk of request) body += chunk;
      const reply = await fetch(origin + request.url, { method: "POST", headers: {
        cookie: cookies, "content-type": "application/json", "sec-fetch-site": "same-origin"
      }, body });
      response.writeHead(reply.status, { "content-type": "application/json" });
      response.end(await reply.text());
      return;
    }
    if (request.url === "/fixture.js") { response.setHeader("Content-Type", "text/javascript"); response.end(bundle.outputFiles[0].contents); return; }
    if (request.url === "/client.css") { response.setHeader("Content-Type", "text/css"); response.end(css); return; }
    response.setHeader("Content-Type", "text/html; charset=utf-8");
    response.end('<!doctype html><html lang="en" data-theme="light"><head><title>Report settings smoke</title><link rel="stylesheet" href="/client.css"></head><body><main><div id="notice" role="status"></div><div id="error" role="alert"></div><div id="host"></div></main><script type="module" src="/fixture.js"></script></body></html>');
  } catch (error) { response.writeHead(500); response.end(String(error)); }
});
await new Promise(resolve => server.listen(0, "127.0.0.1", resolve));
const executablePath = process.env.CHROME_BIN ?? (process.platform === "darwin"
  ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "/usr/bin/google-chrome");
let browser, page;
const stamp = { host: hostname(), utc: new Date().toISOString(), node: process.version };
try {
  browser = await chromium.launch({ executablePath, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"] });
  stamp.browser = await browser.version();
  page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
  const errors = [];
  page.on("pageerror", error => errors.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => window.fixtureReady === true);
  await page.getByRole("searchbox", { name: "Search settings" }).fill("Barcode printer");
  const setting = page.getByLabel("Barcode printer resolution (DPI)", { exact: true });
  assert.equal(await setting.inputValue(), "300");
  await setting.fill("203");
  await setting.press("Tab");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.locator("#notice").filter({ hasText: "Settings saved." }).waitFor();
  await page.reload();
  await page.waitForFunction(() => window.fixtureReady === true);
  await page.getByRole("searchbox", { name: "Search settings" }).fill("Barcode printer");
  assert.equal(await setting.inputValue(), "203");
  await setting.fill("0");
  await setting.press("Tab");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page.locator("#error").filter({ hasText: "base.report_barcode_dpi takes a whole number from 72 to 2400." }).waitFor();
  await page.screenshot({ path: path.join(output, "invalid.png"), fullPage: true });
  await page.reload();
  await page.waitForFunction(() => window.fixtureReady === true);
  await page.getByRole("searchbox", { name: "Search settings" }).fill("Barcode printer");
  assert.equal(await setting.inputValue(), "203", "rejected value must not replace the saved resolution");
  await page.screenshot({ path: path.join(output, "saved.png"), fullPage: true });
  assert.deepEqual(errors, []);
  const result = { ...stamp, status: "PASS", component: "actual Settings component", backend: "actual native RPC and database", save_reload: true, named_validation: true, invalid_write_preserves_value: true };
  await writeFile(path.join(output, "result.json"), JSON.stringify(result, null, 2) + "\n");
  console.log(JSON.stringify(result));
} catch (error) {
  await writeFile(path.join(output, "failure.json"), JSON.stringify({ ...stamp, status: "FAIL", error: String(error) }, null, 2) + "\n");
  if (page) await page.screenshot({ path: path.join(output, "failure.png"), fullPage: true });
  throw error;
} finally {
  if (browser) await browser.close();
  await new Promise(resolve => server.close(resolve));
}
