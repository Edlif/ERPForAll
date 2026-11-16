/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The dashboard card widget: the contract v1 parser and its refusals, the drawing of
// buttons, figures and both chart types, the action each control opens, keyboard use on a kanban card, and the path
// through the application to the action named by its XML id.
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { HumanApp } from "../src/client/app.js";
import { dashboardCardWidget, parseDashboardCard } from "../src/client/dashboard-card.js";
import { CurrencyDisplayCache } from "../src/client/field-currency.js";
import { kanbanLayout, renderKanbanCards } from "../src/client/kanban.js";
import { describeViews, type Field } from "../src/client/metadata.js";
import { configureClientTemplates, fieldWidgets, resolveFieldWidget, type ActionRequest, type WidgetEnvironment } from "../src/client/registry.js";
import { RpcClient } from "../src/client/transport.js";
import "../src/client/fields.js";
import { auditComponent } from "../../../../web/testing/axe.js";

const american = { decimal_point: ".", thousands_sep: ",", grouping: [3, 0] };
const reply = (value: unknown, status = 200) => new Response(JSON.stringify(value), { status, headers: { "Content-Type": "application/json" } });
const contract = () => ({
  v: 1,
  buttons: [
    { label: "Upload", action: "account.action_move_in_invoice_type", view_mode: null, context: {}, primary: false },
    { label: "New Invoice", action: "account.action_move_out_invoice_type", view_mode: "form", context: { default_move_type: "out_invoice", default_journal_id: 5 }, primary: true }
  ],
  stats: [
    { label: "Late", count: 3, amount: "1250000.00", currency_id: 12, action: "account.action_move_out_invoice_type",
      domain: [["journal_id", "=", 5], ["state", "=", "posted"]], context: { journal_type: "sale" }, tone: "danger" },
    { label: "To check", count: 1200, amount: null, currency_id: null, action: "account.action_move_journal_line", domain: [], context: {}, tone: "neutral" }
  ],
  chart: { type: "bar", label: "Due", points: [{ label: "Overdue", value: "900000.00", tone: "danger" }, { label: "This week", value: "1250.50", tone: "neutral" }, { label: "Later", value: "-20", tone: "warning" }] }
});
const json = (change?: (value: ReturnType<typeof contract> & Record<string, unknown>) => void): string => {
  const value = contract() as ReturnType<typeof contract> & Record<string, unknown>;
  change?.(value);
  return JSON.stringify(value);
};
const currencyRpc = () => new RpcClient(async () => reply([{ id: 12, symbol: "Rp", position: "before", decimal_places: 2 }]), () => {});
function environment(value: unknown, extra: Partial<WidgetEnvironment> = {}) {
  const opened: ActionRequest[] = [], reported: unknown[] = [];
  const env: WidgetEnvironment = {
    field: { name: "dashboard_card", kind: "text", label: "Dashboard", required: false, readonly: true } as Field, value, change: () => undefined,
    rpc: currencyRpc(), context: {}, run: job => { void job(); }, report: error => { reported.push(error); }, numericLocale: american,
    currencies: new CurrencyDisplayCache(), viewContext: "card", openAction: async request => { opened.push(request); }, ...extra
  };
  return { env, opened, reported };
}

afterEach(() => { document.body.replaceChildren(); });

// ---- the parser ----

test("contract v1 is admitted into a validated card", () => {
  expect(parseDashboardCard(json())).toEqual({
    buttons: [
      { label: "Upload", action: "account.action_move_in_invoice_type", viewMode: null, context: {}, primary: false },
      { label: "New Invoice", action: "account.action_move_out_invoice_type", viewMode: "form", context: { default_move_type: "out_invoice", default_journal_id: 5 }, primary: true }
    ],
    stats: [
      { label: "Late", count: 3, amount: "1250000.00", currencyId: 12, action: "account.action_move_out_invoice_type",
        domain: [["journal_id", "=", 5], ["state", "=", "posted"]], context: { journal_type: "sale" }, tone: "danger" },
      { label: "To check", count: 1200, amount: null, currencyId: null, action: "account.action_move_journal_line", domain: [], context: {}, tone: "neutral" }
    ],
    chart: { type: "bar", label: "Due", points: [{ label: "Overdue", value: "900000.00", tone: "danger" }, { label: "This week", value: "1250.50", tone: "neutral" }, { label: "Later", value: "-20", tone: "warning" }] }
  });
  expect(parseDashboardCard(json(value => { value.chart = null as never; }))?.chart).toBeNull();
});

test("a record without dashboard data draws an empty card body, not an error", () => {
  for (const value of [null, undefined, false, ""]) {
    expect(parseDashboardCard(value)).toBeNull();
    const { node } = dashboardCardWidget(environment(value).env);
    expect(node.className).toBe("dashboard-card dashboard-card-empty");
    expect(node.childElementCount).toBe(0);
  }
});

test("each departure from contract v1 is refused by the key it concerns and drawn on the card", () => {
  type Card = ReturnType<typeof contract> & Record<string, unknown>;
  const cases: [string | unknown, string][] = [
    [json(value => { value.v = 2; }), "Dashboard card: v 2 is not a known version (expected 1)."],
    [json(value => { delete (value as Partial<Card>).v; }), "Dashboard card: v null is not a known version (expected 1)."],
    [json(value => { delete (value as Partial<Card>).buttons; }), "Dashboard card: card.buttons is missing."],
    [json(value => { value["title"] = "x"; }), "Dashboard card: card.title is not part of contract v1."],
    [json(value => { delete (value.stats[0] as Partial<Card["stats"][0]>).tone; }), "Dashboard card: stats[0].tone is missing."],
    [json(value => { (value.stats[0] as Record<string, unknown>)["count"] = "3"; }), "Dashboard card: stats[0].count must be a whole number of zero or more."],
    [json(value => { (value.stats[0] as Record<string, unknown>)["count"] = -1; }), "Dashboard card: stats[0].count must be a whole number of zero or more."],
    [json(value => { value.stats[0]!.tone = "info"; }), "Dashboard card: stats[0].tone must be neutral, warning or danger."],
    [json(value => { value.chart.points[1]!.tone = "success"; }), "Dashboard card: chart.points[1].tone must be neutral, warning or danger."],
    [json(value => { value.chart.type = "pie"; }), "Dashboard card: chart.type must be \"bar\" or \"line\"."],
    [json(value => { (value.stats[0] as Record<string, unknown>)["amount"] = 1250; }), "Dashboard card: stats[0].amount must be a decimal text such as \"1250.00\"."],
    [json(value => { value.stats[0]!.amount = "1e5"; }), "Dashboard card: stats[0].amount must be a decimal text such as \"1250.00\"."],
    [json(value => { value.chart.points[0]!.value = "nine"; }), "Dashboard card: chart.points[0].value must be a decimal text such as \"1250.00\"."],
    [json(value => { value.stats[0]!.amount = null as never; }), "Dashboard card: stats[0].currency_id must be null exactly when amount is null."],
    [json(value => { (value.buttons[0] as Record<string, unknown>)["view_mode"] = "kanban"; }), "Dashboard card: buttons[0].view_mode must be \"form\", \"list\" or null."],
    [json(value => { (value.buttons[0] as Record<string, unknown>)["primary"] = "yes"; }), "Dashboard card: buttons[0].primary must be true or false."],
    [json(value => { value.buttons[1]!.action = "action_move_out_invoice_type"; }), "Dashboard card: buttons[1].action must be a window action XML id (module.name)."],
    [json(value => { value.buttons[1]!.label = " "; }), "Dashboard card: buttons[1].label must be a non-empty text."],
    [json(value => { (value.stats[0] as Record<string, unknown>)["domain"] = [[1, 2]]; }), "Dashboard card: stats[0].domain[0] must be an operator or a [field, operator, value] condition."],
    [json(value => { (value.stats[0] as Record<string, unknown>)["context"] = []; }), "Dashboard card: stats[0].context must be an object."],
    [json(value => { (value as Record<string, unknown>)["stats"] = {}; }), "Dashboard card: stats must be a list."],
    ["{not json", "Dashboard card: the value is not valid JSON."],
    [{ v: 1 }, "Dashboard card: the value must be JSON text."]
  ];
  for (const [value, message] of cases) {
    expect(() => parseDashboardCard(value), message).toThrowError(expect.objectContaining({ kind: "server", message }));
    const { node } = dashboardCardWidget(environment(value).env);
    expect(node.className).toBe("dashboard-card-error");
    expect(node.textContent).toBe(message);
  }
});

// ---- drawing ----

test("buttons come primary first as house buttons; figures show count, label, amount in its currency and tone", async () => {
  const { env } = environment(json());
  const { node } = dashboardCardWidget(env);
  document.body.append(node);
  const buttons = [...node.querySelectorAll<HTMLButtonElement>(".dashboard-card-buttons button")];
  expect(buttons.map(button => button.textContent)).toEqual(["New Invoice", "Upload"]);
  expect(buttons.map(button => button.className)).toEqual(["erp-button erp-button-primary erp-button-small", "erp-button erp-button-secondary erp-button-small"]);
  const stats = [...node.querySelectorAll<HTMLButtonElement>(".dashboard-card-stat")];
  expect(stats.map(stat => stat.classList.contains("dashboard-card-tone-danger"))).toEqual([true, false]);
  expect(stats[1]!.textContent).toBe("1,200To check");
  await vi.waitFor(() => expect(stats[0]!.querySelector(".dashboard-card-stat-amount")!.getAttribute("aria-busy")).toBeNull());
  expect(stats[0]!.textContent).toBe("3LateRp1,250,000.00");
  expect(stats[0]!.querySelector(".field-currency-symbol")!.textContent).toBe("Rp");
  await auditComponent(node); // throws on a serious or critical finding
});

test("an unreadable currency leaves the figure readable and reports why", async () => {
  const refused = new RpcClient(async () => reply({ name: "erp.exceptions.AccessError", message: "You may not read currencies." }, 403), () => {});
  const { env, reported } = environment(json(), { rpc: refused });
  const { node } = dashboardCardWidget(env);
  await vi.waitFor(() => expect(reported).toHaveLength(1));
  expect(node.querySelector(".dashboard-card-stat-amount")!.textContent).toBe("Amount unavailable");
  expect(node.querySelector(".dashboard-card-stat")!.textContent).toContain("3Late");
});

test("the bar chart draws one bar per point in its tone, with the points as text for assistive technology", () => {
  const { node } = dashboardCardWidget(environment(json()).env);
  const figure = node.querySelector("figure.dashboard-card-chart")!;
  expect(figure.querySelector("figcaption")!.textContent).toBe("Due");
  const drawing = figure.querySelector("svg")!;
  expect(drawing.getAttribute("aria-hidden")).toBe("true");
  expect([...drawing.querySelectorAll("rect")].map(bar => bar.getAttribute("class"))).toEqual(["graph-bar dashboard-card-tone-danger", "graph-bar dashboard-card-tone-neutral", "graph-bar dashboard-card-tone-warning"]);
  expect(drawing.querySelector("polyline")).toBeNull();
  expect([...figure.querySelectorAll(".dashboard-card-chart-axis span")].map(item => item.textContent)).toEqual(["Overdue", "This week", "Later"]);
  expect([...figure.querySelectorAll("ul.visually-hidden li")].map(item => item.textContent)).toEqual(["Overdue: 900,000 (danger)", "This week: 1,250.5", "Later: -20 (warning)"]);
});

test("the line chart draws one line through every point, with the same text alternative", () => {
  const { node } = dashboardCardWidget(environment(json(value => { value.chart.type = "line"; })).env);
  const drawing = node.querySelector("figure.dashboard-card-chart svg")!;
  expect(drawing.getAttribute("class")).toBe("dashboard-card-chart-line");
  expect(drawing.querySelectorAll("rect")).toHaveLength(0);
  expect(drawing.querySelector("polyline")!.getAttribute("points")!.split(" ")).toHaveLength(3);
  expect(node.querySelectorAll("figure ul.visually-hidden li")).toHaveLength(3);
});

test("a chart without points says so", () => {
  const { node } = dashboardCardWidget(environment(json(value => { value.chart.points = []; })).env);
  expect(node.querySelector(".dashboard-card-chart-empty")!.textContent).toBe("No figures for this period.");
  expect(node.querySelector("figure svg")).toBeNull();
});

// ---- behaviour ----

test("a button opens its action with its context; a figure opens its action's list with its exact domain and context", () => {
  const { env, opened } = environment(json());
  const { node } = dashboardCardWidget(env);
  node.querySelector<HTMLButtonElement>(".dashboard-card-buttons .erp-button-primary")!.click();
  node.querySelector<HTMLButtonElement>(".dashboard-card-buttons .erp-button-secondary")!.click();
  node.querySelector<HTMLButtonElement>(".dashboard-card-stat")!.click();
  expect(opened).toEqual([
    { xmlId: "account.action_move_out_invoice_type", viewMode: "form", domain: [], context: { default_move_type: "out_invoice", default_journal_id: 5 } },
    { xmlId: "account.action_move_in_invoice_type", viewMode: null, domain: [], context: {} },
    { xmlId: "account.action_move_out_invoice_type", viewMode: "list", domain: [["journal_id", "=", 5], ["state", "=", "posted"]], context: { journal_type: "sale" } }
  ]);
});

test("on a kanban card the controls are keyboard reachable buttons, and using one does not also open the card's record", async () => {
  const { env, opened } = environment(json());
  const description = describeViews({
    wire_major: 1, registry_version: "r1", description_version: "d1", model: "t.journal", notices: [],
    fields: { "t.journal": { name: { type: "char", label: "Name", required: false, readonly: false, relation: null, relation_field: null, selection: null },
      dashboard: { type: "text", label: "Dashboard", required: false, readonly: true, relation: null, relation_field: null, selection: null } } },
    views: [{ identity: { kind: "stored", id: 7 }, type: "kanban", schema_major: 1, renderer: "web.kanban", root: 0, nodes: [
      { kind: "kanban", children: [1], quick_create: true, records_draggable: true, groups_draggable: true, archivable: true, can_open: true },
      { kind: "kanban_card", children: [2, 3] },
      { kind: "field", force_save: false, field: { model: "t.journal", name: "name" }, widget: "char", options: {}, modifiers: { readonly: null, required: null, invisible: null }, label: "Name" },
      { kind: "field", force_save: false, field: { model: "t.journal", name: "dashboard" }, widget: "dashboard_card", options: {}, modifiers: { readonly: null, required: null, invisible: null }, label: "Dashboard" }
    ] }]
  }, "t.journal", "r1").views.get("kanban")!;
  const records = [{ id: 5, name: "Customer Invoices", dashboard: json() }];
  const cardOpened: number[] = [];
  const grid = renderKanbanCards({ layout: kanbanLayout(description), records, open: id => cardOpened.push(id), renderField: (field, record) => {
    if (field.widget !== "dashboard_card") { const node = document.createElement("span"); node.textContent = String(record[field.name]); return { node, dispose: () => undefined }; }
    return resolveFieldWidget(field.widget, "card").render({ ...env, field, value: record[field.name], values: record });
  } });
  document.body.append(grid.node);
  const card = grid.node.querySelector<HTMLElement>(".kanban-card")!;
  const controls = [...card.querySelectorAll<HTMLButtonElement>(".dashboard-card button")];
  expect(controls).toHaveLength(4); // two buttons, two figures
  for (const control of controls) {
    expect(control.type).toBe("button");
    expect(control.tabIndex).toBe(0);
    expect(control.disabled).toBe(false);
  }
  const stat = card.querySelector<HTMLButtonElement>(".dashboard-card-stat")!;
  stat.focus();
  expect(document.activeElement).toBe(stat);
  // Enter on a control belongs to the control: the card's own Enter (open the record) does not fire.
  stat.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  expect(cardOpened).toEqual([]);
  stat.click(); // what the browser does for Enter or Space on a native button
  expect(opened.map(request => request.viewMode)).toEqual(["list"]);
  expect(cardOpened).toEqual([]);
  card.querySelector<HTMLButtonElement>(".dashboard-card-buttons .erp-button-primary")!.click();
  expect(opened.map(request => request.viewMode)).toEqual(["list", "form"]);
  expect(cardOpened).toEqual([]);
  // The card itself still opens its record.
  card.querySelector<HTMLElement>(".kanban-card-title")!.click();
  expect(cardOpened).toEqual([5]);
  await auditComponent(grid.node);
  grid.dispose();
});

test("the widget name is admitted for text fields; outside a kanban card it says where it belongs", () => {
  expect(fieldWidgets.get("card.dashboard_card").kinds).toEqual(["text"]);
  expect(resolveFieldWidget("dashboard_card", "card")).toBe(fieldWidgets.get("card.dashboard_card"));
  const { node } = resolveFieldWidget("dashboard_card", "form").render(environment(json()).env);
  expect(node.textContent).toBe("The dashboard card widget is drawn on kanban cards only.");
});

test("without an action opener the card shows why instead of drawing dead controls", () => {
  const { node } = dashboardCardWidget(environment(json(), { openAction: undefined }).env);
  expect(node.textContent).toBe("The dashboard card opens actions only on a kanban card.");
});

// ---- through the application ----

const column = (type: string, label: string) => ({ type, label, required: false, readonly: false, relation: null, relation_field: null, selection: null });
const modifiers = { readonly: null, required: null, invisible: null };
const field = (model: string, name: string, widget: string, label: string) => ({ kind: "field", force_save: false, field: { model, name }, widget, options: {}, modifiers, label });
const search = (id: number) => ({ identity: { kind: "stored", id }, type: "search", schema_major: 1, renderer: "web.search", root: 0, nodes: [{ kind: "search", children: [] }] });
const journals = {
  wire_major: 1, registry_version: "r1", description_version: "d1", model: "t.journal", notices: [],
  fields: { "t.journal": { name: column("char", "Name"), dashboard: column("text", "Dashboard") } },
  views: [{ identity: { kind: "stored", id: 7 }, type: "kanban", schema_major: 1, renderer: "web.kanban", root: 0, nodes: [
    { kind: "kanban", children: [1], quick_create: true, records_draggable: true, groups_draggable: true, archivable: true, can_open: true },
    { kind: "kanban_card", children: [2, 3] }, field("t.journal", "name", "char", "Name"), field("t.journal", "dashboard", "dashboard_card", "Dashboard")
  ] }, search(4)]
};
const moves = {
  wire_major: 1, registry_version: "r1", description_version: "d1", model: "t.move", notices: [],
  fields: { "t.move": { name: column("char", "Number") } },
  views: [
    { identity: { kind: "stored", id: 12 }, type: "list", schema_major: 1, renderer: "web.list", root: 0, nodes: [{ kind: "list", children: [1] }, field("t.move", "name", "char", "Number")] },
    { identity: { kind: "stored", id: 13 }, type: "form", schema_major: 1, renderer: "web.form", root: 0, nodes: [{ kind: "form", record_mode: "record", children: [1] }, field("t.move", "name", "char", "Number")] },
    search(14)
  ]
};
const bootstrap = {
  wire_major: 1, registry_version: "r1", asset_version: "a1",
  identity: { uid: 2, name: "Operator", internal: true },
  companies: [{ id: 1, name: "Company" }], current_company_id: 1, allowed_company_ids: [1], active_company_ids: [1],
  locale: { lang: "en_US", tz: "UTC", date_format: "%m/%d/%Y", time_format: "%H:%M:%S", ...american },
  csrf: { logout: "test-token-not-secret" }
};
let app: HumanApp | undefined;
beforeEach(() => {
  configureClientTemplates([{ name: "web.HumanControlPanel", render: () => {
    const root = document.createElement("section");
    root.setAttribute("data-control-panel", "");
    for (const key of ["breadcrumbs", "search", "actions", "extension", "tools"]) { const node = document.createElement("div"); node.setAttribute(`data-control-${key}`, ""); root.append(node); }
    return root;
  } }]);
});
afterEach(() => { app?.dispose(); app = undefined; vi.restoreAllMocks(); vi.unstubAllGlobals(); history.replaceState(null, "", "/"); });

test("through the application a figure loads its action by XML id and opens its list with the action's domain, the figure's domain and context", async () => {
  history.replaceState(null, "", "/erp/action-20");
  const calls: { model: string; method: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, init: RequestInit) => {
    const parts = url.split("/"), method = parts.at(-1)!, model = parts.at(-2)!, body = init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    calls.push({ model, method, body });
    if (method === "erp_load_web_menus") return reply({ wire_major: 1, registry_version: "r1", roots: [10], nodes: [{ module: null, id: 10, parent_id: null, label: "Journals", action_id: 20, children: [], icon: null }] });
    if (method === "erp_load_web_action" || method === "erp_load_web_action_views") {
      const action = { wire_major: 1, id: 20, name: "Journals", model: "t.journal", target: "current", views: [{ id: 7, type: "kanban" }], domain: null, context: null };
      return reply(method === "erp_load_web_action_views" ? { action, views: journals } : action);
    }
    if (method === "erp_load_web_action_ref") {
      // The action's own domain, as the server sends an action domain: an expression the client evaluates.
      const domain = { schema: 1, nodes: [{ kind: "literal", value: { kind: "str", value: "move_type" } }, { kind: "literal", value: { kind: "str", value: "=" } },
        { kind: "literal", value: { kind: "str", value: "out_invoice" } }, { kind: "tuple", items: [0, 1, 2] }, { kind: "list", items: [3] }] };
      return reply({ wire_major: 1, id: 30, name: "Invoices", model: "t.move", target: "current", views: [{ id: 12, type: "list" }, { id: 13, type: "form" }], domain, context: null });
    }
    if (method === "get_views") return reply(model === "t.move" ? moves : journals);
    if (method === "fields_get") return reply({ name: { string: "Name", type: "char", groupable: true } });
    if (method === "web_search_read") return reply(model === "t.move" ? { length: 0, records: [] } : { length: 1, records: [{ id: 5, name: "Customer Invoices", dashboard: json() }] });
    if (model === "res.currency" && method === "web_read") return reply([{ id: 12, symbol: "Rp", position: "before", decimal_places: 2 }]);
    return reply([]);
  }));
  const root = document.createElement("div");
  for (const region of ["header", "navigation", "notice", "content"]) { const node = document.createElement("div"); node.setAttribute(`data-client-${region}`, ""); root.append(node); }
  document.body.append(root);
  app = new HumanApp(root, bootstrap);
  await app.start();
  await vi.waitFor(() => expect(root.querySelector(".dashboard-card-stat")).not.toBeNull());
  root.querySelector<HTMLButtonElement>(".dashboard-card-stat")!.click();
  await vi.waitFor(() => expect(calls.some(call => call.model === "t.move" && call.method === "web_search_read")).toBe(true));
  const reference = calls.find(call => call.method === "erp_load_web_action_ref")!;
  expect(reference.model).toBe("ir.actions.act_window");
  expect(reference.body).toMatchObject({ xml_id: "account.action_move_out_invoice_type" });
  const read = calls.filter(call => call.model === "t.move" && call.method === "web_search_read").at(-1)!.body;
  expect(read["domain"]).toEqual([["move_type", "=", "out_invoice"], ["journal_id", "=", 5], ["state", "=", "posted"]]);
  expect(read["context"]).toMatchObject({ journal_type: "sale" });
  expect(new URL(window.location.href).searchParams.get("view_type")).toBe("list");
});

test("the action's own literal defaults travel with a figure's list, so New there creates with them", async () => {
  history.replaceState(null, "", "/erp/action-20");
  const calls: { model: string; method: string; body: Record<string, unknown> }[] = [];
  vi.stubGlobal("fetch", vi.fn(async (url: string, init: RequestInit) => {
    const parts = url.split("/"), method = parts.at(-1)!, model = parts.at(-2)!, body = init.body ? JSON.parse(String(init.body)) as Record<string, unknown> : {};
    calls.push({ model, method, body });
    if (method === "erp_load_web_menus") return reply({ wire_major: 1, registry_version: "r1", roots: [10], nodes: [{ module: null, id: 10, parent_id: null, label: "Journals", action_id: 20, children: [], icon: null }] });
    if (method === "erp_load_web_action" || method === "erp_load_web_action_views") {
      const action = { wire_major: 1, id: 20, name: "Journals", model: "t.journal", target: "current", views: [{ id: 7, type: "kanban" }], domain: null, context: null };
      return reply(method === "erp_load_web_action_views" ? { action, views: journals } : action);
    }
    if (method === "erp_load_web_action_ref") {
      // `{'default_move_type': 'out_invoice'}` as the server sends an admitted action context.
      const context = { schema: 1, nodes: [{ kind: "literal", value: { kind: "str", value: "default_move_type" } },
        { kind: "literal", value: { kind: "str", value: "out_invoice" } }, { kind: "dict", items: [[0, 1]] }] };
      return reply({ wire_major: 1, id: 30, name: "Invoices", model: "t.move", target: "current", views: [{ id: 12, type: "list" }, { id: 13, type: "form" }], domain: null, context });
    }
    if (method === "get_views") return reply(model === "t.move" ? moves : journals);
    if (method === "fields_get") return reply({ name: { string: "Name", type: "char", groupable: true } });
    if (method === "web_search_read") return reply(model === "t.move" ? { length: 0, records: [] } : { length: 1, records: [{ id: 5, name: "Customer Invoices", dashboard: json() }] });
    if (method === "default_get") return reply({ name: "" });
    if (method === "onchange") return reply({ value: {} });
    if (model === "res.currency" && method === "web_read") return reply([{ id: 12, symbol: "Rp", position: "before", decimal_places: 2 }]);
    return reply([]);
  }));
  const root = document.createElement("div");
  for (const region of ["header", "navigation", "notice", "content"]) { const node = document.createElement("div"); node.setAttribute(`data-client-${region}`, ""); root.append(node); }
  document.body.append(root);
  app = new HumanApp(root, bootstrap);
  await app.start();
  await vi.waitFor(() => expect(root.querySelector(".dashboard-card-stat")).not.toBeNull());
  root.querySelector<HTMLButtonElement>(".dashboard-card-stat")!.click();
  await vi.waitFor(() => expect(calls.some(call => call.model === "t.move" && call.method === "web_search_read")).toBe(true));
  const read = calls.filter(call => call.model === "t.move" && call.method === "web_search_read").at(-1)!.body;
  expect(read["context"]).toMatchObject({ default_move_type: "out_invoice", journal_type: "sale" });
  const create = [...root.querySelectorAll<HTMLButtonElement>("[data-control-actions] button")].find(node => node.textContent?.trim() === "New");
  expect(create).toBeDefined();
  create!.click();
  await vi.waitFor(() => expect(calls.some(call => call.model === "t.move" && call.method === "default_get")).toBe(true));
  const defaults = calls.find(call => call.model === "t.move" && call.method === "default_get")!.body;
  expect(defaults["context"]).toMatchObject({ default_move_type: "out_invoice" });
});
