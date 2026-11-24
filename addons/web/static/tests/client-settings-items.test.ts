/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, expect, test, vi } from "vitest";
import { settingsView } from "../src/client/services.js";
import { RpcClient } from "../src/client/transport.js";
import type { SettingsItem } from "../src/generated/human-ui.js";

const info: SettingsItem = { kind: "info", key: "base.users", label: "Active Users", app: "base", block: "Users", sequence: 10, value: 0, detail: null };
const link: SettingsItem = { kind: "link", key: "base.manage", label: "Manage Users", app: "base", block: "Users", sequence: 20, target: { kind: "action", id: 7, res_id: null } };
const setting = { key: "notes.enabled", label: "Enabled", kind: "boolean", choices: null, writable: true, app: "notes", block: "General", sequence: 100 };
let refuseRun = false;
afterEach(() => { refuseRun = false; document.body.replaceChildren(); });
async function mount(items: unknown, declarations: unknown[] = [], initialApp: string | null = null, currentCompanyId?: number, sharedOnly = false) {
  const calls: { method: string; args: Record<string, unknown> }[] = [], openTarget = vi.fn(async () => {});
  const rpc = new RpcClient(async (path, init) => {
    const args = JSON.parse(String(init?.body)) as Record<string, unknown>, method = String(path).split("/").at(-1)!;
    calls.push({ method, args });
    const sharedScope = !sharedOnly || (args["scope"] as {kind?: string} | undefined)?.kind === "global";
    const available = sharedScope ? declarations : [];
    const response = method === "erp_describe" ? { wire_major: 1, registry_version: "r1", scope: args["scope"], declarations: available, ...(items === undefined ? {} : { items }) }
      : { wire_major: 1, scope: args["scope"], revision: "revision-one", values: available.map(declaration => ({ key: (declaration as { key: string }).key, value: false, source: "stored" })) };
    return new Response(JSON.stringify(response), { headers: { "Content-Type": "application/json" } });
  }, () => {});
  const host = document.createElement("div"), panel = document.createElement("div"); panel.innerHTML = '<div data-control-search></div>'; document.body.append(host, panel);
  const jobs: Promise<void>[] = [];
  const controller = await settingsView(host, rpc, [{ id: 1, name: "One" }], job => { if (!refuseRun) jobs.push(job()); }, () => {}, {}, ["global", "company"], "r1", undefined, panel, initialApp,
    { navigation: { ...(currentCompanyId === undefined ? {} : { currentCompanyId }), roots: [{ id: 1, module: "base", label: "Administration", icon: "lucide:settings" }], openTarget } });
  return { host, panel, controller, calls, openTarget, jobs };
}

/// (partial: client rendering, state isolation and callback dispatch)
test("General info and links render without editable declarations and never enter values RPCs", async () => {
  const { host, controller, calls, openTarget, jobs } = await mount([link, info]);
  expect(host.querySelector(".settings-empty")).toBeNull();
  expect(host.querySelector('[data-setting-key="base.users"]')?.textContent).toContain("0");
  expect(host.querySelector(".settings-section-title")?.textContent).toContain("General Settings");
  expect(host.querySelector(".settings-app-name")?.textContent).toBe("Administration");
  expect(host.querySelector('.settings-app-icon [data-icon="settings"], .settings-app-icon[data-icon="settings"]')).not.toBeNull();
  const target = host.querySelector<HTMLButtonElement>('[data-setting-key="base.manage"] button')!;
  expect(target.type).toBe("button"); target.click(); await Promise.all(jobs);
  expect(openTarget).toHaveBeenCalledExactlyOnceWith(link.target);
  expect(controller.dirty()).toBe(false); await controller.save();
  expect(calls.find(call => call.method === "erp_read_values")?.args["keys"]).toEqual([]);
  expect(calls.some(call => call.method === "erp_write_values")).toBe(false);
});

/// (partial: explicit selection, safe text and search)
test("mixed settings retain explicit app selection and search safe multiline item text", async () => {
  const company = { ...info, key: "base.company", label: "Company", value: "<img src=x> 東京", detail: "First street\nSecond street" };
  const { host, panel, calls } = await mount([company, link], [setting], "notes");
  expect(host.querySelector<HTMLElement>('[data-app-section="notes"]')?.style.display).toBe("");
  expect(host.querySelector<HTMLElement>('[data-app-section="base"]')?.style.display).toBe("none");
  expect(host.querySelector("img")).toBeNull();
  const search = panel.querySelector<HTMLInputElement>("input")!; search.value = "second street"; search.dispatchEvent(new Event("input"));
  expect(host.querySelector<HTMLElement>('[data-app-section="base"]')?.style.display).toBe("");
  expect(host.querySelector<HTMLElement>('[data-setting-key="base.company"]')?.textContent).toContain("Second street");
  // A text value shows without its label; the label stays in the document for assistive technology.
  expect(host.querySelector('[data-setting-key="base.company"] .setting-title-row .setting-label-text')?.classList.contains("visually-hidden")).toBe(true);
  // A box and the link under it are found together: by the box's text and by the link's name.
  expect(host.querySelector<HTMLElement>('[data-setting-key="base.manage"]')?.style.display).toBe("");
  search.value = "manage users"; search.dispatchEvent(new Event("input"));
  expect(host.querySelector<HTMLElement>('[data-setting-key="base.company"]')?.style.display).toBe("");
  search.value = "base.manage"; search.dispatchEvent(new Event("input"));
  expect(host.querySelector<HTMLElement>('[data-setting-key="base.company"]')?.style.display).toBe("");
  search.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  expect(host.querySelector<HTMLElement>('[data-app-section="notes"]')?.style.display).toBe("");
  expect(calls.find(call => call.method === "erp_read_values")?.args["keys"]).toEqual(["notes.enabled"]);
});

test("a link with no information item before it keeps its own box", async () => {
  const { host } = await mount([link]);
  expect(host.querySelector('.setting-block[data-setting-key="base.manage"] > .setting-right-pane > button')).not.toBeNull();
  expect(host.querySelector(".settings-panel-links")).toBeNull();
});

test("General is first by default and links/infos interleave by sequence without controls", async () => {
  const { host } = await mount([link, { ...info, sequence: 30 }, { ...info, key: "base.first", sequence: 5, value: false }], [setting]);
  expect(host.querySelector(".settings-app-tab")?.getAttribute("data-app")).toBe("base");
  expect([...host.querySelectorAll('[data-app-section="base"] [data-setting-key]')].map(el => el.getAttribute("data-setting-key"))).toEqual(["base.first", "base.manage", "base.users"]);
  expect(host.querySelector('[data-setting-key="base.first"]')?.textContent).toContain("false");
  // The link sits under the information item it follows, in that item's box; the next item starts a new box.
  expect(host.querySelector('[data-setting-key="base.first"] .settings-panel-links [data-setting-key="base.manage"] button')).not.toBeNull();
  expect(host.querySelector('[data-setting-key="base.users"] .settings-panel-links')).toBeNull();
  expect(host.querySelector('[data-setting-key="base.users"] .setting-title-row .setting-label-text')?.classList.contains("visually-hidden")).toBe(false);
  expect(host.querySelector('[data-app-section="base"] input')).toBeNull();
});

test("older descriptions can omit items and forbidden row keys are never invented", async () => {
  const { host } = await mount(undefined, [setting]);
  expect(host.querySelector('[data-app-section="base"]')).toBeNull();
  expect(host.querySelector('[data-setting-key="notes.enabled"]')).not.toBeNull();
});

test.each([
  [null, "Invalid setting items."],
  [[{ ...info, surprise: true }], "Unknown setting item member: surprise."],
  [[{ ...info, detail: undefined }], "Missing setting item member: detail."],
  [[{ ...info, kind: "unknown" }], "Unsupported setting item kind: unknown."],
  [[info, info], "Duplicate settings item or declaration key: base.users."],
  [[{ ...link, target: { kind: "menu", id: 7, res_id: null } }], "Unknown setting target member: res_id."],
  [[{ ...link, target: { kind: "action", id: 0, res_id: null } }], "Invalid setting target id."],
  [[{ ...link, target: { kind: "action", id: 7 } }], "Missing setting target member: res_id."],
])("malformed panel metadata fails loudly (%j)", async (items, message) => {
  await expect(mount(items)).rejects.toMatchObject({ kind: message.startsWith("Unknown") || message.startsWith("Unsupported") ? "unsupported" : "server", message });
});

test("scope changes and Discard reload information and record targets together", async () => {
  const revisions = new Map<number, number>(); const targets: unknown[] = [], jobs: Promise<void>[] = [];
  const rpc = new RpcClient(async (path, init) => {
    const args = JSON.parse(String(init?.body)), company = args.scope.kind === "company" ? args.scope.company_id : 1;
    const describe = String(path).endsWith("erp_describe");
    const revision = (revisions.get(company) ?? 0) + Number(describe);
    revisions.set(company, revision);
    const payload = describe ? { wire_major: 1, registry_version: "r1", scope: args.scope, declarations: [], items: [{ ...info, value: `Company ${company} revision ${revision}` }, { ...link, target: { kind: "action", id: 7, res_id: company } }] }
      : { wire_major: 1, scope: args.scope, revision: String(revision), values: [] };
    return new Response(JSON.stringify(payload), { headers: { "Content-Type": "application/json" } });
  }, () => {});
  const host = document.createElement("div"); document.body.append(host);
  const controller = await settingsView(host, rpc, [{ id: 2, name: "Two" }], job => { jobs.push(job()); }, () => {}, {}, ["global", "company"], "r1", undefined, undefined, null, { navigation: { roots: [], openTarget: async target => { targets.push(target); } } });
  host.querySelector<HTMLButtonElement>('[data-scope="2"]')!.click(); await Promise.all(jobs);
  expect(host.querySelector('[data-setting-key="base.users"]')?.textContent).toContain("Company 2 revision 2");
  host.querySelector<HTMLButtonElement>('[data-setting-key="base.manage"] button')!.click(); await Promise.all(jobs);
  expect(targets).toEqual([{ kind: "action", id: 7, res_id: 2 }]);
  [...host.querySelectorAll<HTMLButtonElement>(".settings-toolbar button")].find(button => button.textContent === "Discard")!.click(); await Promise.all(jobs);
  expect(host.querySelector('[data-setting-key="base.users"]')?.textContent).toContain("Company 2 revision 3");
  controller.dispose?.();
  host.querySelector<HTMLButtonElement>('[data-setting-key="base.manage"] button')!.click();
  expect(targets).toHaveLength(1);
});

// Review S1: a refused `run` (busy screen) never starts the job, so no link may stay disabled.
test("a link click that run refuses leaves every link enabled", async () => {
  const { host, jobs, openTarget } = await mount([link, { ...link, key: "base.other", label: "Other" }]);
  refuseRun = true;
  const links = [...host.querySelectorAll<HTMLButtonElement>("[data-setting-link]")];
  links[0]!.click();
  expect(links.every(button => !button.disabled)).toBe(true);
  refuseRun = false; links[0]!.click(); await Promise.all(jobs);
  expect(openTarget).toHaveBeenCalledOnce();
});

// Review N6a: a text info value has the caption byte limit and control-character check.
test("a text info value is bounded and refuses control characters; an empty one is allowed", async () => {
  await expect(mount([{ ...info, value: "x".repeat(1025) }])).rejects.toThrow("Invalid setting item value.");
  await expect(mount([{ ...info, value: "a\u0007b" }])).rejects.toThrow("Invalid setting item value.");
  const { host } = await mount([{ ...info, value: "" }]);
  expect(host.querySelector('[data-setting-key="base.users"]')).not.toBeNull();
});


/// (partial: unavailable information is visible and excluded from client scalar requests)
/// (partial: inactive-consumer help is rendered beside the editable value)
test("Inventory keeps deferred features visible and outside scalar saves", async () => {
  const stockInfo: SettingsItem = { kind: "info", key: "stock.group_stock_multi_locations", label: "Multiple locations", app: "stock", block: "Warehouse", sequence: 100, value: "Multiple locations (unavailable)", detail: "Unavailable until a later release supplies feature-group changes." };
  const fleetInfo: SettingsItem = { kind: "info", key: "stock.module_stock_fleet", label: "Transport", app: "stock", block: "Operations", sequence: 100, value: "Transport (unavailable)", detail: "Unavailable until a later release supplies this addon." };
  const stockLink: SettingsItem = { kind: "link", key: "stock.module_delivery", label: "Set up carriers in Addons", app: "stock", block: "Shipping", sequence: 200, target: { kind: "action", id: 77, res_id: null } };
  const stockSetting = { ...setting, key: "stock.stock_move_email_validation", app: "stock", block: "Shipping", default: false, help: "Saved only; delivery email is not sent by this setting yet." };
  const { host, controller, calls, openTarget } = await mount([stockInfo, fleetInfo, stockLink], [stockSetting], "stock");
  const unavailable = host.querySelector('[data-setting-key="stock.group_stock_multi_locations"]')!;
  expect(unavailable.querySelector(".settings-info-value")?.textContent).toBe("Multiple locations (unavailable)");
  expect(unavailable.textContent).toContain("a later release");
  expect(unavailable.querySelector("input, select")).toBeNull();
  const fleet = host.querySelector('[data-setting-key="stock.module_stock_fleet"]')!;
  expect(fleet.querySelector(".settings-info-value")?.textContent).toBe("Transport (unavailable)");
  expect(fleet.querySelector("button")).toBeNull();
  expect(host.querySelector<HTMLButtonElement>('[data-setting-key="stock.module_delivery"] button')).not.toBeNull();
  expect(host.textContent).toContain(stockSetting.help);
  expect(calls.find(call => call.method === "erp_read_values")?.args["keys"]).toEqual([stockSetting.key]);
  const input = host.querySelector<HTMLInputElement>('[data-setting-key="stock.stock_move_email_validation"] input')!;
  input.checked = true; input.dispatchEvent(new Event("change"));
  expect(controller.dirty()).toBe(true);
  await controller.save();
  const write = calls.find(call => call.method === "erp_write_values")!;
  expect(write.args["changes"]).toEqual([{ key: stockSetting.key, value: true }]);
  host.querySelector<HTMLButtonElement>('[data-setting-key="stock.module_delivery"] button')!.click();
  await vi.waitFor(() => expect(openTarget).toHaveBeenCalledWith({ kind: "action", id: 77, res_id: null }));
  expect(calls.some(call => call.method === "button_immediate_install")).toBe(false);
});

test("an addon settings menu opens its active company while the general menu opens shared settings", async () => {
  const scoped = await mount([], [setting], "notes", 1);
  expect(scoped.calls.find(call => call.method === "erp_describe")?.args["scope"]).toEqual({ kind: "company", company_id: 1 });
  expect(scoped.host.querySelector('[aria-checked=true]')?.textContent).toBe("Current company");
  const shared = await mount([], [setting], null, 1);
  expect(shared.host.querySelector('[aria-checked=true]')?.getAttribute("data-scope")).toBe("global");
});

test("shared-only addon settings read just the shared values when entered from a company", async () => {
  const { host, calls } = await mount([], [setting], "notes", 1, true);
  expect(host.querySelector('[data-scope="global"]')?.getAttribute("aria-checked")).toBe("true");
  expect(calls.filter(c => c.method === "erp_read_values").map(c => c.args["scope"])).toEqual([{kind: "global"}]);
});

function stubDialogs(): void {
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
    configurable: true,
    value: function (this: HTMLDialogElement) { this.open = true; },
  });
  Object.defineProperty(HTMLDialogElement.prototype, "close", {
    configurable: true,
    value: function (this: HTMLDialogElement) { this.open = false; this.dispatchEvent(new Event("close")); },
  });
}

test("module toggles render all three states, keep unavailable disabled, and stay out of scalar RPCs", async () => {
  const installedItem: SettingsItem = { kind: "module", key: "stock.module_delivery", label: "Carriers", app: "stock", block: "Shipping", sequence: 200, addon: "delivery", state: "installed" };
  const notInstalledItem: SettingsItem = { kind: "module", key: "stock.module_stock_sms", label: "Text delivery", app: "stock", block: "Shipping", sequence: 210, addon: "stock_sms", state: "not_installed" };
  const unavailableItem: SettingsItem = { kind: "module", key: "stock.module_product_expiry", label: "Expiry dates", app: "stock", block: "Traceability", sequence: 220, addon: "product_expiry", state: "not_available" };
  const { host, controller, calls, jobs } = await mount([installedItem, notInstalledItem, unavailableItem], [], "stock");

  const installedBox = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input[type="checkbox"]')!;
  const notInstalledBox = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_stock_sms"] input[type="checkbox"]')!;
  const unavailableRow = host.querySelector<HTMLElement>('[data-setting-key="stock.module_product_expiry"]')!;
  const unavailableBox = unavailableRow.querySelector<HTMLInputElement>('input[type="checkbox"]')!;

  expect(installedBox.checked).toBe(true);
  expect(installedBox.disabled).toBe(false);
  expect(notInstalledBox.checked).toBe(false);
  expect(notInstalledBox.disabled).toBe(false);
  expect(unavailableBox.checked).toBe(false);
  expect(unavailableBox.disabled).toBe(true);
  expect(unavailableRow.classList.contains("is-unavailable")).toBe(true);
  expect(unavailableRow.textContent).toContain("Not available in this server");

  // Unavailable row remains disabled after another operation (Discard reload) completes.
  [...host.querySelectorAll<HTMLButtonElement>(".settings-toolbar button")].find(b => b.textContent === "Discard")!.click();
  await Promise.all(jobs);
  expect(host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_product_expiry"] input[type="checkbox"]')!.disabled).toBe(true);

  expect(controller.dirty()).toBe(false);
  expect(calls.find(c => c.method === "erp_read_values")?.args["keys"]).toEqual([]);
  expect(calls.some(c => c.method === "erp_write_values")).toBe(false);
});

test("module toggle install and uninstall flows use existing endpoints and update in place without page reload", async () => {
  stubDialogs();
  let state: "not_installed" | "installed" = "not_installed";
  let registryVersion = "r1";
  const notices: string[] = [];
  const posts: { path: string; body: Record<string, unknown> }[] = [];
  const previewCalls: Record<string, unknown>[] = [];
  const jobs: Promise<void>[] = [];

  const rpc = new RpcClient(async (path, init) => {
    const url = String(path);
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    if (url === "/erp/module/install") {
      posts.push({ path: url, body });
      state = "installed";
      registryVersion = "r2";
      return new Response(JSON.stringify({ installed: ["delivery"], registry_version: registryVersion, asset_version: "a2" }), { headers: { "Content-Type": "application/json" } });
    }
    if (url === "/erp/module/uninstall") {
      posts.push({ path: url, body });
      state = "not_installed";
      registryVersion = "r3";
      return new Response(JSON.stringify({ uninstalled: ["delivery"], registry_version: registryVersion, asset_version: "a3" }), { headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/erp_preview_uninstall")) {
      previewCalls.push(body);
      return new Response(JSON.stringify({
        fingerprint: "fp-123",
        resolved: ["delivery"],
        dropped_models: [{ model: "delivery.carrier", table: "delivery_carrier", row_count: "2" }],
        target_count: "2",
      }), { headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/erp_describe")) {
      return new Response(JSON.stringify({
        wire_major: 1,
        registry_version: registryVersion,
        scope: body["scope"],
        declarations: [],
        items: [{ kind: "module", key: "stock.module_delivery", label: "Carriers", app: "stock", block: "Shipping", sequence: 200, addon: "delivery", state }],
      }), { headers: { "Content-Type": "application/json" } });
    }
    return new Response(JSON.stringify({ wire_major: 1, scope: body["scope"], revision: "1", values: [] }), { headers: { "Content-Type": "application/json" } });
  }, () => {});

  const host = document.createElement("div");
  document.body.append(host);
  await settingsView(host, rpc, [{ id: 1, name: "One" }], job => { jobs.push(job()); }, message => { notices.push(message); }, {}, ["global"], "r1", undefined, undefined, "stock");

  // 1. Tick -> Cancel install: no POST, checkbox stays unchecked.
  const box1 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  expect(box1.checked).toBe(false);
  box1.checked = true;
  box1.dispatchEvent(new Event("change"));
  expect(box1.checked).toBe(false);
  await vi.waitFor(() => expect(document.querySelector(".message-dialog-footer button")).not.toBeNull());
  [...document.querySelectorAll<HTMLButtonElement>(".message-dialog-footer button")].find(b => b.textContent === "Cancel")!.click();
  await Promise.all(jobs);
  expect(posts).toHaveLength(0);
  expect(host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!.checked).toBe(false);

  // 2. Tick -> Confirm install: POST /erp/module/install, reloads with r2, checkbox becomes checked.
  const box2 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  box2.checked = true;
  box2.dispatchEvent(new Event("change"));
  await vi.waitFor(() => expect(document.querySelector(".message-dialog-footer button")).not.toBeNull());
  [...document.querySelectorAll<HTMLButtonElement>(".message-dialog-footer button")].find(b => b.textContent === "Install")!.click();
  await Promise.all(jobs);
  expect(posts).toEqual([{ path: "/erp/module/install", body: { modules: ["delivery"] } }]);
  expect(host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!.checked).toBe(true);
  expect(notices).toContain("Carriers installed.");

  // 3. Untick -> Cancel uninstall impact dialog: preview called, no uninstall POST, stays checked.
  const box3 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  box3.checked = false;
  box3.dispatchEvent(new Event("change"));
  expect(box3.checked).toBe(true);
  await vi.waitFor(() => expect(document.querySelector(".addon-uninstall-dialog")).not.toBeNull());
  expect(previewCalls).toHaveLength(1);
  expect(document.querySelector(".addon-uninstall-dialog")?.textContent).toContain("delivery.carrier");
  document.querySelector<HTMLButtonElement>(".addon-uninstall-cancel")!.click();
  await Promise.all(jobs);
  expect(posts).toHaveLength(1);
  expect(host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!.checked).toBe(true);

  // 4. Untick -> Confirm uninstall: POST /erp/module/uninstall with fingerprint, reloads with r3, becomes unchecked.
  const box4 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  box4.checked = false;
  box4.dispatchEvent(new Event("change"));
  await vi.waitFor(() => expect(document.querySelector(".addon-uninstall-confirm")).not.toBeNull());
  document.querySelector<HTMLButtonElement>(".addon-uninstall-confirm")!.click();
  await Promise.all(jobs);
  expect(posts[1]).toEqual({ path: "/erp/module/uninstall", body: { modules: ["delivery"], impact: "fp-123" } });
  expect(host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!.checked).toBe(false);
  expect(notices).toContain("Carriers uninstalled.");
});

test("module toggle handles uninstall preview validation refusal and 409 conflict", async () => {
  stubDialogs();
  let mode: "validation" | "conflict" = "validation";
  const notices: string[] = [];
  const errors: unknown[] = [];
  const jobs: Promise<void>[] = [];

  const rpc = new RpcClient(async (path, init) => {
    const url = String(path);
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    if (url.endsWith("/erp_preview_uninstall")) {
      if (mode === "validation") {
        return new Response(JSON.stringify({ error: { message: "Refused", name: "erp.exceptions.ValidationError" } }), { status: 400, headers: { "Content-Type": "application/json" } });
      }
      return new Response(JSON.stringify({ fingerprint: "fp-stale", resolved: ["delivery"], dropped_models: [], target_count: "0" }), { headers: { "Content-Type": "application/json" } });
    }
    if (url === "/erp/module/uninstall") {
      return new Response(JSON.stringify({ error: { message: "Stale fingerprint", name: "erp.exceptions.ConflictError" } }), { status: 409, headers: { "Content-Type": "application/json" } });
    }
    if (url.endsWith("/erp_describe")) {
      return new Response(JSON.stringify({
        wire_major: 1,
        registry_version: "r1",
        scope: body["scope"],
        declarations: [],
        items: [{ kind: "module", key: "stock.module_delivery", label: "Carriers", app: "stock", block: "Shipping", sequence: 200, addon: "delivery", state: "installed" }],
      }), { headers: { "Content-Type": "application/json" } });
    }
    return new Response(JSON.stringify({ wire_major: 1, scope: body["scope"], revision: "1", values: [] }), { headers: { "Content-Type": "application/json" } });
  }, () => {});

  const host = document.createElement("div");
  document.body.append(host);
  await settingsView(host, rpc, [{ id: 1, name: "One" }], job => { jobs.push(job().catch(err => { errors.push(err); })); }, message => { notices.push(message); }, {}, ["global"], "r1", undefined, undefined, "stock");

  // Validation refusal on preview: shows fixed notice, no dialog, no thrown error.
  const box1 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  box1.checked = false;
  box1.dispatchEvent(new Event("change"));
  await Promise.all(jobs);
  expect(document.querySelector(".addon-uninstall-dialog")).toBeNull();
  expect(notices).toContain("Uninstalling this addon from this page is not available yet.");
  expect(errors).toHaveLength(0);

  // 409 conflict on uninstall: shows conflict notice and reloads settings.
  mode = "conflict";
  const box2 = host.querySelector<HTMLInputElement>('[data-setting-key="stock.module_delivery"] input')!;
  box2.checked = false;
  box2.dispatchEvent(new Event("change"));
  await vi.waitFor(() => expect(document.querySelector(".addon-uninstall-confirm")).not.toBeNull());
  document.querySelector<HTMLButtonElement>(".addon-uninstall-confirm")!.click();
  await Promise.all(jobs);
  expect(notices).toContain("Database state changed since preview. A fresh confirmation is required.");
  expect(errors).toHaveLength(1);
});
