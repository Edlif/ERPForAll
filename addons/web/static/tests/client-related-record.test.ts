/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { configureClientTemplates } from "../src/client/registry.js";
import { HumanApp } from "../src/client/app.js";

// A record opened from a form: one server call resolves its action, and the new form goes on top of the old one.

const bootstrap = {
  wire_major: 1, registry_version: "r1", asset_version: "a1",
  identity: { uid: 2, name: "Operator", internal: true },
  companies: [{ id: 1, name: "Company" }], current_company_id: 1, allowed_company_ids: [1], active_company_ids: [1],
  locale: { lang: "en_US", tz: "UTC", date_format: "%m/%d/%Y", time_format: "%H:%M:%S" },
  csrf: { logout: "test-token-not-secret" }
};

const modifiers = { readonly: null, required: null, invisible: null };
const char = (model: string) => ({ kind: "field", force_save: false, field: { model, name: "name" }, widget: "char", options: {}, modifiers, label: "Name" });
const link = (name: string, label: string) => ({ kind: "field", force_save: false, field: { model: "test.note", name }, widget: "many2one", options: {}, modifiers, label });
const relation = (label: string, target: string) => ({ type: "many2one", label, required: false, readonly: true, relation: target, relation_field: null, selection: null });
const nameField = { type: "char", label: "Name", required: true, readonly: false, relation: null, relation_field: null, selection: null };

const noteViews = {
  wire_major: 1, registry_version: "r1", description_version: "n1", model: "test.note", notices: [],
  fields: { "test.note": { name: nameField, company_id: relation("Company", "res.company"), parent_id: relation("Parent", "test.note") } },
  views: [
    { identity: { kind: "stored", id: 1 }, type: "form", schema_major: 1, renderer: "web.form", root: 0,
      nodes: [{ kind: "form", record_mode: "record", children: [1, 2, 3] }, char("test.note"), link("company_id", "Company"), link("parent_id", "Parent")] },
    { identity: { kind: "stored", id: 2 }, type: "list", schema_major: 1, renderer: "web.list", root: 0,
      nodes: [{ kind: "list", children: [1] }, char("test.note")] }
  ]
};
const companyViews = {
  wire_major: 1, registry_version: "r1", description_version: "c1", model: "res.company", notices: [],
  fields: { "res.company": { name: nameField } },
  views: [{ identity: { kind: "stored", id: 3 }, type: "form", schema_major: 1, renderer: "web.form", root: 0,
    nodes: [{ kind: "form", record_mode: "record", children: [1] }, char("res.company")] }]
};
const notesAction = { wire_major: 1, id: 20, name: "Notes", model: "test.note", target: "current", views: [{ id: 2, type: "list" }, { id: 1, type: "form" }], domain: null, context: null };
const companiesAction = { wire_major: 1, id: 21, name: "Companies", model: "res.company", target: "current", views: [{ id: 3, type: "form" }], domain: null, context: null };
const notes: Record<number, Record<string, unknown>> = {
  5: { id: 5, name: "Source record", company_id: { id: 9, display_name: "Company record" }, parent_id: { id: 6, display_name: "Parent record" } },
  6: { id: 6, name: "Parent record", company_id: false, parent_id: false }
};

let app: HumanApp | undefined;
const showModal = Object.getOwnPropertyDescriptor(HTMLDialogElement.prototype, "showModal");

beforeEach(() => {
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", { configurable: true, value(this: HTMLDialogElement) { this.open = true; } });
  configureClientTemplates([{
    name: "web.HumanControlPanel",
    render: () => {
      const root = document.createElement("section");
      root.setAttribute("data-control-panel", "");
      for (const key of ["breadcrumbs", "search", "actions", "extension", "tools"]) {
        const node = document.createElement("div");
        node.setAttribute(`data-control-${key}`, "");
        root.append(node);
      }
      return root;
    }
  }]);
});

afterEach(() => {
  app?.dispose();
  app = undefined;
  if (showModal) Object.defineProperty(HTMLDialogElement.prototype, "showModal", showModal);
  vi.restoreAllMocks();
  document.body.replaceChildren();
  vi.unstubAllGlobals();
  history.replaceState(null, "", "/");
});

/** Starts on the notes list and returns every request path (`model/method`) the client sends. */
async function start(resolve: (model: string) => unknown = model => model === "res.company"
  ? { menu: 11, any_record: true, action: companiesAction, views: companyViews }
  : { menu: 10, any_record: true, action: notesAction, views: noteViews }): Promise<{ root: HTMLElement; calls: string[] }> {
  const calls: string[] = [];
  vi.stubGlobal("fetch", vi.fn(async (path: string, init: RequestInit) => {
    const body = JSON.parse(String(init.body ?? "{}")) as Record<string, unknown>;
    const call = path.split("/").slice(-2).join("/");
    calls.push(call);
    let value: unknown = [];
    if (call.endsWith("erp_load_web_menus")) value = { wire_major: 1, registry_version: "r1", roots: [10, 11], nodes: [
      { module: null, id: 10, parent_id: null, label: "Notes", action_id: 20, children: [], icon: null },
      { module: null, id: 11, parent_id: null, label: "Companies", action_id: 21, children: [], icon: null }] };
    else if (call.endsWith("erp_resolve_record_action")) value = resolve(String(body["model"]));
    else if (call.endsWith("erp_load_web_action_views")) value = (body["ids"] as number[])[0] === 21 ? { action: companiesAction, views: companyViews } : { action: notesAction, views: noteViews };
    else if (call === "test.note/web_search_read") value = { length: 2, records: [{ id: 5, name: "Source record" }, { id: 6, name: "Parent record" }] };
    else if (call === "test.note/web_read") value = [notes[(body["ids"] as number[])[0]!]];
    else if (call === "res.company/web_read") value = [{ id: 9, name: "Company record" }];
    return new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
  }));
  history.replaceState(null, "", "/erp/action-20");
  const root = document.createElement("div");
  for (const region of ["header", "navigation", "notice", "content"]) {
    const node = document.createElement("div");
    node.setAttribute(`data-client-${region}`, "");
    root.append(node);
  }
  document.body.append(root);
  app = new HumanApp(root, bootstrap);
  await app.start();
  root.querySelector<HTMLElement>("tbody tr[data-id='5']")!.click();
  await vi.waitFor(() => expect(root.querySelector<HTMLInputElement>('input[name="name"]')?.value).toBe("Source record"));
  return { root, calls };
}

const opened = (root: HTMLElement, name: string) => vi.waitFor(() => expect(root.querySelector<HTMLInputElement>('input[name="name"]')?.value).toBe(name));
const open = (root: HTMLElement, label: string) => root.querySelector<HTMLButtonElement>(`[aria-label="Open ${label}"]`)!.click();
/** The trail as drawn: the older entries (oldest first), the previous entry, the current one. */
const trail = (root: HTMLElement) => {
  const crumbs = root.querySelector("[data-control-breadcrumbs]")!;
  const older = [...crumbs.querySelectorAll(".breadcrumb-dropdown-item")].map(item => item.textContent).reverse();
  const previous = crumbs.querySelector(".breadcrumb-previous .breadcrumb-back")?.textContent;
  return [...older, ...(previous ? [previous] : []), crumbs.querySelector(".breadcrumb-current")?.textContent];
};
const metadata = (calls: string[]) => calls.filter(call => /erp_load_web_action|erp_resolve_record_action|get_views|search_count/.test(call));

test("a first related open costs one resolve call and the record read; a second open of the model only the read", async () => {
  const { root, calls } = await start();
  calls.length = 0;
  open(root, "Company: Company record");
  await opened(root, "Company record");
  expect(location.pathname).toBe("/erp/action-21");
  expect(new URLSearchParams(location.search).get("menu")).toBe("11");
  expect(metadata(calls)).toEqual(["ir.actions.act_window/erp_resolve_record_action"]);
  expect(calls.filter(call => call === "res.company/web_read")).toHaveLength(1);

  root.querySelector<HTMLButtonElement>(".breadcrumb-previous .breadcrumb-back")!.click();
  await opened(root, "Source record");
  calls.length = 0;
  open(root, "Company: Company record");
  await opened(root, "Company record");
  expect(metadata(calls)).toEqual([]);
  expect(calls.filter(call => call === "res.company/web_read")).toHaveLength(1);
});

test("a match that holds for one record only is resolved again for the next open", async () => {
  const { root, calls } = await start(model => model === "res.company"
    ? { menu: 11, any_record: false, action: companiesAction, views: companyViews } : null);
  open(root, "Company: Company record");
  await opened(root, "Company record");
  root.querySelector<HTMLButtonElement>(".breadcrumb-previous .breadcrumb-back")!.click();
  await opened(root, "Source record");
  calls.length = 0;
  open(root, "Company: Company record");
  await opened(root, "Company record");
  // The action's views stay cached; only the record's own check runs again.
  expect(metadata(calls)).toEqual(["ir.actions.act_window/erp_resolve_record_action"]);
});

test("no accessible action keeps the source form and reports which model could not be opened", async () => {
  const { root } = await start(() => null);
  open(root, "Company: Company record");
  await vi.waitFor(() => expect(document.querySelector("dialog.client-error-dialog")?.textContent).toContain("No accessible form action is available for res.company."));
  expect(location.pathname).toBe("/erp/action-20");
  expect(root.querySelector<HTMLInputElement>('input[name="name"]')?.value).toBe("Source record");
});

test("the trail leads from the opened record back to the record it was opened from", async () => {
  const { root } = await start();
  expect(trail(root)).toEqual(["Notes", "Source record"]);
  open(root, "Company: Company record");
  await opened(root, "Company record");
  expect(trail(root)).toEqual(["Notes", "Source record", "Company record"]);

  root.querySelector<HTMLButtonElement>(".breadcrumb-previous .breadcrumb-back")!.click();
  await opened(root, "Source record");
  expect(location.pathname).toBe("/erp/action-20");
  expect(new URLSearchParams(location.search).get("id")).toBe("5");
  expect(trail(root)).toEqual(["Notes", "Source record"]);
});

test("a record of the same model opened from a form keeps both records in the trail", async () => {
  const { root } = await start();
  open(root, "Parent: Parent record");
  await opened(root, "Parent record");
  expect(location.pathname).toBe("/erp/action-20");
  expect(new URLSearchParams(location.search).get("id")).toBe("6");
  expect(trail(root)).toEqual(["Notes", "Source record", "Parent record"]);

  root.querySelector<HTMLButtonElement>(".breadcrumb-previous .breadcrumb-back")!.click();
  await opened(root, "Source record");
  expect(trail(root)).toEqual(["Notes", "Source record"]);
});

test("browser Back from the opened record lands on the source record with the trail cut to it", async () => {
  const { root } = await start();
  open(root, "Company: Company record");
  await opened(root, "Company record");
  history.back();
  await opened(root, "Source record");
  expect(location.pathname).toBe("/erp/action-20");
  expect(trail(root)).toEqual(["Notes", "Source record"]);
});
