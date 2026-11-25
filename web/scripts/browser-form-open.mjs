/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Form open time in a real browser against a real installed server and the compiled client. No route is
// intercepted. It reports numbers; it asserts only that each step reached the page it should (no timing budget).
//
// Input (stdin, JSON): origin, cookies, quotations action, orders {zero, fifty, many, invoiced} (ids), the
// invoice id, and line counts. Environment: FORM_OPEN_OUTPUT (required, a directory), FORM_OPEN_REPS (samples
// per scenario, default 7, 3 to 50), FORM_OPEN_HOST_LABEL (the host name written to the result; default the
// machine's own name), FORM_OPEN_REVISION and FORM_OPEN_PROFILE (written as given), FORM_OPEN_CPU_PROFILE (set: one
// more 500-line open under the CPU profiler, its profile written next to the result and its costliest functions in it).
//
// A sample is the page clock from the click (a capture listener) to the first animation frame in which the
// destination is on the page (its route in the address bar, which the client writes only after it put the
// new page in the document, and the page's own content). Each request the page makes is recorded with its
// start relative to the click, its time to first byte and total time (the server reports no own timing, so
// on loopback the time to first byte stands for the server's time), and its body size.
import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { cpus, hostname, loadavg, release } from "node:os";
import path from "node:path";
import { chromium } from "playwright";

let input = "";
for await (const chunk of process.stdin) input += chunk;
const { origin, cookies, action, orders, invoice, lines } = JSON.parse(input);
const output = process.env.FORM_OPEN_OUTPUT;
if (!output) throw new Error("FORM_OPEN_OUTPUT is required");
const reps = Number(process.env.FORM_OPEN_REPS ?? "7");
if (!Number.isSafeInteger(reps) || reps < 3 || reps > 50) throw new Error("FORM_OPEN_REPS must be a whole number from 3 to 50");
await mkdir(output, { recursive: true });
const executablePath = process.env.CHROME_BIN ?? (process.platform === "darwin"
  ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" : "/usr/bin/google-chrome");
const browser = await chromium.launch({ executablePath, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"] });
const result = {
  tool: "web/scripts/browser-form-open.mjs", host: process.env.FORM_OPEN_HOST_LABEL || hostname(), kernel: release(),
  cpus: cpus().length, utc_start: new Date().toISOString(), loadavg_start: loadavg(), node: process.version,
  browser: browser.version(), revision: process.env.FORM_OPEN_REVISION ?? null, profile: process.env.FORM_OPEN_PROFILE ?? null,
  reps, lines, samples: [], summary: {}, status: "failed",
};

const median = values => { const s = [...values].sort((a, b) => a - b), m = s.length >> 1; return s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2; };
const round = value => Math.round(value * 10) / 10;
function callName(url) {
  const { pathname } = new URL(url);
  const json = /^\/erp\/json\/2\/([^/]+)\/([^/]+)$/.exec(pathname);
  return json ? `${json[1]}.${json[2]}` : pathname;
}

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
  // every finished request of the page, with its wall-clock start (epoch ms, as the click's)
  const finished = [];
  let inFlight = 0;
  page.on("request", () => { inFlight += 1; });
  page.on("requestfailed", request => { inFlight -= 1; finished.push({ request, failed: request.failure()?.errorText ?? "failed" }); });
  page.on("requestfinished", request => { inFlight -= 1; finished.push({ request }); });
  await page.addInitScript(() => {
    addEventListener("click", () => { window.__formOpenClick = performance.timeOrigin + performance.now(); }, { capture: true });
  });
  const formRoute = id => `${origin}/erp/action-${action}?id=${id}&view_type=form`;
  const listRoute = `${origin}/erp/action-${action}`;
  const settle = async () => {
    // the page is idle once no request was in flight for half a second (at most 20 s)
    const deadline = Date.now() + 20000;
    let quietSince = Date.now();
    while (Date.now() < deadline) {
      if (inFlight > 0) quietSince = Date.now();
      else if (Date.now() - quietSince >= 500) return;
      await new Promise(resolve => setTimeout(resolve, 25));
    }
    throw new Error("the page did not become idle within 20 s");
  };
  const load = async url => { await page.goto(url, { waitUntil: "networkidle" }); await settle(); };

  // The destination is on the page: its route is in the address bar and its page is in the document.
  // want: {kind: "form", id} a form of that record, {kind: "other-form"} a form without the customer link
  // (the customer's own form), {kind: "list"} a list with rows.
  // Runs in the page (Playwright sends its text, so it closes over nothing).
  const arrived = want => {
    const url = new URL(location.href), form = url.searchParams.get("view_type") === "form" && document.querySelector("form.record-form") !== null;
    const there = want.kind === "form" ? form && url.searchParams.get("id") === want.id
      : want.kind === "other-form" ? form && document.querySelector('button[aria-label^="Open Customer"]') === null
      : !url.searchParams.has("id") && document.querySelector(".list-table tbody tr") !== null;
    if (!there || window.__formOpenClick === undefined) return false;
    window.__formOpenPaint ??= performance.timeOrigin + performance.now();
    return true;
  };
  const reach = want => page.waitForFunction(arrived, want, { polling: "raf", timeout: 60000 });
  // One sample: click `target`, wait for the animation frame in which the destination is on the page.
  async function sample(scenario, rep, target, want) {
    await settle();
    const first = finished.length;
    await page.evaluate(() => { window.__formOpenClick = undefined; window.__formOpenPaint = undefined; });
    await target.click();
    await reach(want);
    const clicked = await page.evaluate(() => window.__formOpenClick), painted = await page.evaluate(() => window.__formOpenPaint);
    await settle();
    const requests = [];
    for (const { request, failed } of finished.slice(first)) {
      const timing = request.timing(), sizes = failed ? null : await request.sizes();
      const response = failed ? null : await request.response();
      requests.push({
        call: callName(request.url()), http: request.method(), status: response?.status() ?? null, failed: failed ?? null,
        start_ms: round(timing.startTime - clicked), before_paint: timing.startTime < painted,
        ttfb_ms: timing.responseStart >= 0 && timing.requestStart >= 0 ? round(timing.responseStart - timing.requestStart) : null,
        total_ms: timing.responseEnd >= 0 ? round(timing.responseEnd) : null, bytes: sizes?.responseBodySize ?? null,
      });
    }
    requests.sort((a, b) => a.start_ms - b.start_ms);
    result.samples.push({ scenario, rep, ms: round(painted - clicked), requests });
  }
  const form = id => ({ kind: "form", id: String(id) }), list = { kind: "list" };
  const back = async want => { await page.goBack(); await reach(want); };
  const openRow = id => page.locator(`[data-record-open="${id}"]`).first();
  const customer = () => page.getByRole("button", { name: /^Open Customer(: |$)/ });
  for (let rep = 1; rep <= reps; rep++) {
    // a list row to its form: first in a fresh page (nothing cached), then again after going back
    for (const [name, id] of [["0", orders.zero], [String(lines.fifty), orders.fifty], [String(lines.many), orders.many]]) {
      await load(listRoute);
      await sample(`list_row_to_form_${name}_lines_first`, rep, openRow(id), form(id));
      await back(list);
      await sample(`list_row_to_form_${name}_lines_second`, rep, openRow(id), form(id));
    }
    // the many2one link to the customer, first in a fresh page and again after going back
    await load(formRoute(orders.fifty));
    await sample("many2one_to_form_first", rep, customer(), { kind: "other-form" });
    await back(form(orders.fifty));
    await sample("many2one_to_form_second", rep, customer(), { kind: "other-form" });
    // the count button of an order to its invoice list, then the invoice
    await load(formRoute(orders.invoiced));
    await sample("count_button_to_list", rep, page.locator(".form-button-box button", { hasText: "Invoices" }), list);
    await sample("count_list_row_to_form", rep, openRow(invoice), form(invoice));
  }
  if (process.env.FORM_OPEN_CPU_PROFILE) {
    // one more 500-line open under the page's CPU profiler: the functions that took the most time themselves
    const cdp = await context.newCDPSession(page);
    await cdp.send("Profiler.enable");
    await cdp.send("Profiler.setSamplingInterval", { interval: 100 });
    await load(listRoute);
    await cdp.send("Profiler.start");
    await sample(`profiled_list_row_to_form_${lines.many}_lines_first`, 1, openRow(orders.many), form(orders.many));
    const { profile } = await cdp.send("Profiler.stop");
    await writeFile(path.join(output, "form-open.cpuprofile"), JSON.stringify(profile));
    const byNode = new Map(profile.nodes.map(node => [node.id, node])), self = new Map();
    profile.samples.forEach((id, index) => {
      const { functionName, url, lineNumber } = byNode.get(id).callFrame;
      const key = `${functionName || "(anonymous)"} ${url.split("/").pop()}:${lineNumber + 1}`;
      self.set(key, (self.get(key) ?? 0) + (profile.timeDeltas[index] ?? 0) / 1000);
    });
    result.cpu_self_ms = Object.fromEntries([...self].sort((a, b) => b[1] - a[1]).slice(0, 40).map(([key, ms]) => [key, round(ms)]));
  }

  for (const scenario of [...new Set(result.samples.map(sample => sample.scenario))]) {
    const of = result.samples.filter(sample => sample.scenario === scenario), times = of.map(sample => sample.ms);
    const before = of.map(sample => sample.requests.filter(request => request.before_paint).length);
    const calls = {};
    for (const sample of of) for (const request of sample.requests) {
      const entry = calls[`${request.before_paint ? "before" : "after"} ${request.call}`] ??= { count: 0, ttfb_ms: [], bytes: [] };
      entry.count += 1;
      if (request.ttfb_ms !== null) entry.ttfb_ms.push(request.ttfb_ms);
      if (request.bytes !== null) entry.bytes.push(request.bytes);
    }
    result.summary[scenario] = {
      n: of.length, median_ms: round(median(times)), min_ms: Math.min(...times), max_ms: Math.max(...times),
      requests_before_paint_median: median(before),
      calls: Object.fromEntries(Object.entries(calls).map(([name, entry]) => [name, {
        per_sample: round(entry.count / of.length), median_ttfb_ms: entry.ttfb_ms.length ? round(median(entry.ttfb_ms)) : null,
        median_bytes: entry.bytes.length ? median(entry.bytes) : null,
      }])),
    };
  }
  assert.deepEqual(errors, [], "no uncaught browser errors");
  // Chrome reports net::ERR_ABORTED for some reads whose answer the page went on to use (the form painted with its record;
  // these scenarios make no writes); such reads are listed, not failed. Any other failure or an HTTP error fails the run.
  const aborted = request => request.failed === "net::ERR_ABORTED" && request.http === "POST" && request.status === null;
  result.aborted_reads = result.samples.flatMap(sample => sample.requests.filter(aborted).map(request => `${sample.scenario} #${sample.rep}: ${request.call}`));
  const failed = result.samples.flatMap(sample => sample.requests.filter(request => !aborted(request) && (request.failed || request.status >= 400)).map(request => `${sample.scenario}: ${request.call} ${request.status ?? request.failed}`));
  assert.deepEqual(failed, [], "every request succeeded");
  result.status = "passed";
} catch (error) {
  result.error = error instanceof Error ? error.stack : String(error);
  if (page) {
    const file = path.join(output, "failure.png");
    try { await page.screenshot({ path: file, fullPage: true }); result.failure_screenshot = file; }
    catch (captureError) { result.captureError = String(captureError); }
  }
  throw error;
} finally {
  result.utc_end = new Date().toISOString();
  result.loadavg_end = loadavg();
  await writeFile(path.join(output, "result.json"), `${JSON.stringify(result, null, 2)}\n`);
  console.log(JSON.stringify({ status: result.status, host: result.host, loadavg_start: result.loadavg_start, loadavg_end: result.loadavg_end, summary: result.summary }));
  await browser.close();
}
