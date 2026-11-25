/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSearchPanelSidebar } from "../src/client/search-panel.js";
import { renderHierarchyTree, type HierarchyState } from "../src/client/hierarchy.js";
import { renderActivityMatrix, type ActivityItemRow, type ActivityMatrixOptions } from "../src/client/activity-view.js";
import { kanbanLayout, renderKanbanCards } from "../src/client/kanban.js";
import { fieldControl } from "../src/client/fields.js";
import { DialogService } from "../src/client/dialog-service.js";
import type { Field, Node, View } from "../src/client/metadata.js";
import type { SectionsPanel } from "../src/client/panel-metadata.js";
import { RpcClient } from "../src/client/transport.js";

function field(name: string, kind: Field["kind"], label = name, extra: Partial<Field> = {}): Field {
  return { name, kind, label, required: false, readonly: false, selection: null, relation: null, relationField: null, ...extra };
}
const fieldNode = (f: Field): Node => ({ kind: "field", field: f, label: f.label, widget: f.widget ?? "" });
const unusedRpc = (): RpcClient => new RpcClient(async () => { throw new Error("Unexpected RPC"); }, () => {});

let dialogs: DialogService;
beforeEach(() => {
  Object.defineProperty(HTMLDialogElement.prototype, "showModal", { configurable: true, value(this: HTMLDialogElement) { this.open = true; } });
  Object.defineProperty(HTMLDialogElement.prototype, "close", { configurable: true, value(this: HTMLDialogElement) { this.open = false; this.dispatchEvent(new Event("close")); } });
  dialogs = new DialogService(() => {});
});
afterEach(() => { dialogs.dispose(); vi.restoreAllMocks(); document.body.replaceChildren(); });

const openDialog = (): HTMLDialogElement => {
  const dialog = document.querySelector<HTMLDialogElement>("dialog[data-message-dialog]");
  expect(dialog).not.toBeNull();
  return dialog!;
};
const dialogButton = (label: string): HTMLButtonElement => {
  const found = [...openDialog().querySelectorAll<HTMLButtonElement>("button")].find(b => b.textContent === label);
  expect(found).toBeDefined();
  return found!;
};

describe("SearchPanelSidebar", () => {
  const panel = (): SectionsPanel => ({
    kind: "sections",
    version: 1,
    viewTypes: ["kanban", "list", "hierarchy"],
    notice: null,
    sections: [{
      kind: "category",
      id: 1,
      field: field("department_id", "many2one", "Department", { relation: "hr.department" }),
      label: "Department",
      icon: "users",
      enable_counters: true,
      expand: true,
      hierarchize: true,
      limit: 200,
      default_values: [],
    }],
  });
  const rpc = (): RpcClient => ({
    call: vi.fn(async (_model: string, method: string) => {
      if (method === "search_panel_select_range") {
        return {
          parent_field: "parent_id",
          values: [
            { id: 1, display_name: "Management", __count: 12, parent_id: false },
            { id: 2, display_name: "Engineering", __count: 6, parent_id: [1, "Management"] },
          ],
        };
      }
      return {};
    }),
  }) as unknown as RpcClient;

  it("loads hierarchical category counters and emits child_of domains on click", async () => {
    const onChange = vi.fn();
    const sidebar = createSearchPanelSidebar({ model: "hr.employee", panel: panel(), rpc: rpc(), context: {}, onChange, report: () => {} });

    await sidebar.refresh([]);
    expect(sidebar.element.textContent).toContain("Management");
    expect(sidebar.element.textContent).toContain("Engineering");
    expect(sidebar.element.textContent).toContain("12");

    const engBtn = [...sidebar.element.querySelectorAll<HTMLButtonElement>(".search-panel-pick")]
      .find(b => b.textContent?.includes("Engineering"));
    expect(engBtn).toBeDefined();
    engBtn!.click();

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(sidebar.getDomain()).toEqual([["department_id", "child_of", 2]]);
    sidebar.dispose();
  });

  it("keeps keyboard focus on the picked value after the sections are rebuilt, and marks All as a tree item", async () => {
    const sidebar = createSearchPanelSidebar({ model: "hr.employee", panel: panel(), rpc: rpc(), context: {}, onChange: () => {}, report: () => {} });
    document.body.append(sidebar.element);
    await sidebar.refresh([]);
    const pick = (label: string): HTMLButtonElement => [...sidebar.element.querySelectorAll<HTMLButtonElement>(".search-panel-pick")].find(b => b.textContent?.includes(label))!;
    expect(pick("All").closest("li")!.getAttribute("role")).toBe("treeitem");
    const before = pick("Engineering");
    before.focus();
    before.click();
    const after = pick("Engineering");
    expect(after).not.toBe(before);
    expect(document.activeElement).toBe(after);
    sidebar.dispose();
  });
});

describe("Hierarchy (Org Chart) renderer", () => {
  const nameField = field("name", "char", "Name");
  const jobField = field("job_title", "char", "Job Title");
  const parentField = field("parent_id", "many2one", "Manager", { relation: "hr.employee" });
  const colorField = field("department_color", "integer", "Color");
  const view: View = {
    type: "hierarchy",
    mode: "record",
    fields: [nameField, jobField, parentField, colorField],
    nodes: [{
      kind: "hierarchy",
      parentField,
      childField: null,
      avatarField: null,
      colorField,
      countField: null,
      draggable: true,
      label: "people",
      children: [fieldNode(nameField), fieldNode(jobField)],
    }],
  };
  const records: Record<string, unknown>[] = [
    { id: 1, name: "Nadia Kusuma", job_title: "CEO", parent_id: false, department_color: 5 },
    { id: 2, name: "Budi Santoso", job_title: "VP Engineering", parent_id: { id: 1, display_name: "Nadia Kusuma" }, department_color: 4 },
    { id: 3, name: "Fajar Nugroho", job_title: "Principal Rust", parent_id: { id: 2, display_name: "Budi Santoso" }, department_color: 7 },
    { id: 4, name: "Sari Wulan", job_title: "VP Finance", parent_id: { id: 1, display_name: "Nadia Kusuma" }, department_color: 2 },
  ];
  const formatField = (f: Field, rec: Record<string, unknown>): string => String(rec[f.name] ?? "");
  const card = (node: HTMLElement, id: number): HTMLElement => node.querySelector<HTMLElement>(`.hierarchy-card[data-id="${id}"]`)!;

  it("renders roots, expands children on toggle click, and invokes open callback", () => {
    const opened: number[] = [];
    const tree = renderHierarchyTree({ view, records: records.slice(0, 3), open: id => opened.push(id), formatField });

    expect(tree.node.getAttribute("role")).toBe("tree");
    expect(tree.node.textContent).toContain("Nadia Kusuma");
    expect(tree.node.textContent).toContain("Budi Santoso");
    expect(tree.node.textContent).not.toContain("Fajar Nugroho");

    const budiToggle = tree.node.querySelector<HTMLButtonElement>(".hierarchy-card-toggle:not(.is-open)");
    expect(budiToggle).not.toBeNull();
    budiToggle!.click();
    expect(tree.node.textContent).toContain("Fajar Nugroho");

    const cardEl = tree.node.querySelector<HTMLElement>(".hierarchy-card");
    cardEl!.click();
    expect(opened.length).toBe(1);
    tree.dispose();
  });

  it("opens a card from the keyboard", () => {
    const opened: number[] = [];
    const tree = renderHierarchyTree({ view, records, open: id => opened.push(id), formatField });
    const budi = card(tree.node, 2);
    expect(budi.tabIndex).toBe(0);
    budi.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(opened).toEqual([2]);
    tree.dispose();
  });

  it("reassigns a parent without drag and drop, never offering the card's own subtree", async () => {
    const onReassign = vi.fn(async (_child: number, _parent: number) => {});
    const tree = renderHierarchyTree({ view, records, formatField, onReassign, dialogs });
    document.body.append(tree.node);
    const move = card(tree.node, 2).querySelector<HTMLButtonElement>(".hierarchy-card-move");
    expect(move).not.toBeNull();
    expect(move!.getAttribute("aria-label")).toBe("Change Manager of Budi Santoso");
    move!.click();
    const select = openDialog().querySelector<HTMLSelectElement>("select")!;
    expect([...select.options].map(option => option.value)).toEqual(["1", "4"]);
    expect(select.value).toBe("1");
    select.value = "4";
    dialogButton("Save").click();
    expect(onReassign).toHaveBeenCalledWith(2, 4);
    expect(document.querySelector("dialog[data-message-dialog]")).toBeNull();
    tree.dispose();
  });

  it("has no keyboard reassignment without a dialog service", () => {
    const tree = renderHierarchyTree({ view, records, formatField, onReassign: async () => {} });
    expect(tree.node.querySelector(".hierarchy-card-move")).toBeNull();
    tree.dispose();
  });

  it("keeps what the user unfolded across a reload of the same view", () => {
    const state: HierarchyState = {};
    const first = renderHierarchyTree({ view, records, formatField, state });
    card(first.node, 2).querySelector<HTMLButtonElement>(".hierarchy-card-toggle")!.click();
    expect(first.node.textContent).toContain("Fajar Nugroho");
    first.dispose();
    const second = renderHierarchyTree({ view, records: records.map(rec => ({ ...rec })), formatField, state });
    expect(second.node.textContent).toContain("Fajar Nugroho");
    second.dispose();
  });
});

describe("Activity Matrix renderer", () => {
  const nameField = field("name", "char", "Name");
  const view: View = {
    type: "activity",
    mode: "record",
    fields: [nameField],
    nodes: [{ kind: "activity", avatarField: null, children: [fieldNode(nameField)] }],
  };
  const overdue: ActivityItemRow = {
    id: 101,
    res_id: 10,
    activity_type_id: 1,
    summary: "Quarterly review",
    date_deadline: "2026-11-24",
    state: "overdue",
    user_name: "Admin",
  };
  const matrix = (overrides: Partial<ActivityMatrixOptions> = {}): ReturnType<typeof renderActivityMatrix> => {
    const rendered = renderActivityMatrix({
      model: "hr.employee",
      view,
      records: [{ id: 10, name: "Nadia Kusuma" }, { id: 11, name: "Budi Santoso" }],
      types: [{ id: 1, name: "Call" }, { id: 2, name: "Meeting" }],
      activities: [overdue],
      rpc: unusedRpc(),
      context: {},
      userId: 2,
      formatField: (f, rec) => String(rec[f.name] ?? ""),
      onChanged: async () => {},
      report: () => {},
      dialogs,
      ...overrides,
    });
    document.body.append(rendered.node);
    return rendered;
  };

  it("renders record rows, activity type columns, and colored deadline states", () => {
    const rendered = matrix();
    expect(rendered.node.textContent).toContain("Nadia Kusuma");
    expect(rendered.node.textContent).toContain("Call");
    expect(rendered.node.textContent).toContain("Meeting");
    expect(rendered.node.querySelector(".activity-cell-pill.is-overdue")).not.toBeNull();
    const bar = rendered.node.querySelector(".activity-type-bar")!;
    expect(bar.getAttribute("role")).toBe("img");
    expect(bar.getAttribute("aria-label")).toBe("1 overdue, 0 due today, 0 planned");
    rendered.dispose();
  });

  it("reports a failed reload after Mark done instead of dropping it", async () => {
    const failure = new Error("reload failed");
    const report = vi.fn();
    const call = vi.fn(async (_model: string, _method: string, _params: unknown, _options: unknown): Promise<unknown> => true);
    const rendered = matrix({ rpc: { call } as unknown as RpcClient, onChanged: async () => { throw failure; }, report });
    rendered.node.querySelector<HTMLButtonElement>(".activity-cell-pill")!.click();
    dialogButton("Mark done").click();
    await vi.waitFor(() => expect(report).toHaveBeenCalledWith(failure));
    expect(call).toHaveBeenCalledWith("mail.activity", "action_feedback", expect.objectContaining({ ids: [101] }), { write: true });
    rendered.dispose();
  });

  it("asks for the record when scheduling from the header, in a dialog of the dialog service", async () => {
    const call = vi.fn(async (_model: string, _method: string, _params: unknown, _options: unknown): Promise<unknown> => [500]);
    const onChanged = vi.fn(async () => {});
    const rendered = matrix({ rpc: { call } as unknown as RpcClient, onChanged });
    rendered.node.querySelector<HTMLButtonElement>(".activity-matrix-schedule-btn")!.click();
    const select = openDialog().querySelector<HTMLSelectElement>("select[name=res_id]");
    expect(select).not.toBeNull();
    select!.value = "11";
    dialogButton("Schedule").click();
    await vi.waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
    expect(call).toHaveBeenCalledWith("mail.activity", "create", expect.objectContaining({
      vals_list: [expect.objectContaining({ res_model: "hr.employee", res_id: 11, activity_type_id: 1 })],
    }), { write: true });
    expect(document.querySelector("dialog[data-message-dialog]")).toBeNull();
    rendered.dispose();
  });

  it("closes its dialog on the dialog's cancel (Escape) and asks for no record from a cell", async () => {
    const rendered = matrix();
    rendered.node.querySelector<HTMLButtonElement>(".activity-cell-add")!.click();
    const dialog = openDialog();
    expect(dialog.querySelector("select[name=res_id]")).toBeNull();
    dialog.dispatchEvent(new Event("cancel", { cancelable: true }));
    await vi.waitFor(() => expect(dialog.isConnected).toBe(false));
    rendered.dispose();
  });
});

describe("Kanban cards", () => {
  const kanbanView = (children: Node[]): View => ({
    type: "kanban",
    mode: "record",
    fields: [],
    nodes: [{
      kind: "kanban",
      quickCreate: false,
      recordsDraggable: false,
      groupsDraggable: false,
      archivable: false,
      canOpen: true,
      defaultGroupBy: null,
      children: [{ kind: "kanban_card", children }],
    }],
  });
  const renderField = (f: Field, rec: Record<string, unknown>): { node: HTMLElement; dispose: () => void } => {
    const span = document.createElement("span");
    span.dataset["field"] = f.name;
    span.textContent = String(rec[f.name] ?? "");
    return { node: span, dispose: () => {} };
  };

  it("renders initials tile when image aside is empty and keeps media layout", () => {
    const imgField = field("image_128", "binary", "Photo", { widget: "image" });
    const nameField = field("name", "char", "Name");
    const emailField = field("work_email", "char", "Email", { widget: "email" });
    const layout = kanbanLayout(kanbanView([
      { kind: "kanban_section", region: "aside", children: [fieldNode(imgField)] },
      { kind: "kanban_section", region: "main", children: [fieldNode(nameField), fieldNode(emailField)] },
      { kind: "kanban_section", region: "footer", children: [] },
    ]));
    const rendered = renderKanbanCards({
      layout,
      records: [{ id: 7, name: "Kartika Dewi", work_email: "kartika@example.id", image_128: false }],
      renderField,
    });
    expect(rendered.node.querySelector(".kanban-card-has-media")).not.toBeNull();
    expect(rendered.node.querySelector(".kanban-card-media-initials")?.textContent).toBe("KD");
    rendered.dispose();
  });

  it("colours a card only when the card declares an invisible integer color field", () => {
    const nameField = field("name", "char", "Name");
    const hiddenColor = field("color", "integer", "Color", { invisible: true });
    const record = { id: 3, name: "Retail", color: 3 };
    const declared = renderKanbanCards({ layout: kanbanLayout(kanbanView([fieldNode(nameField), fieldNode(hiddenColor)])), records: [record], renderField });
    expect(declared.node.querySelector(".kanban-card")!.classList.contains("kanban-card-accent-3")).toBe(true);
    declared.dispose();
    const undeclared = renderKanbanCards({ layout: kanbanLayout(kanbanView([fieldNode(nameField)])), records: [record], renderField });
    expect(undeclared.node.querySelector(".kanban-card")!.className).not.toContain("kanban-card-accent");
    undeclared.dispose();
  });

  it("shows a visible color field as an ordinary line", () => {
    const nameField = field("name", "char", "Name");
    const colorField = field("color", "integer", "Color");
    const layout = kanbanLayout(kanbanView([fieldNode(nameField), fieldNode(colorField)]));
    expect(layout.accent).toBeUndefined();
    const rendered = renderKanbanCards({ layout, records: [{ id: 3, name: "Retail", color: 3 }], renderField });
    expect(rendered.node.querySelector("[data-field=color]")?.textContent).toBe("3");
    rendered.dispose();
  });
});

describe("Progress bar field", () => {
  const progress = field("progress", "integer", "Progress", { widget: "progressbar" });
  it("is a progress bar for assistive technology and clears to false when emptied", () => {
    const change = vi.fn();
    const node = fieldControl({ field: progress, value: 40, rpc: unusedRpc(), context: {}, change, run: job => { void job(); }, viewContext: "form" });
    const track = node.querySelector<HTMLElement>(".field-progressbar-track")!;
    expect(track.getAttribute("role")).toBe("progressbar");
    expect(track.getAttribute("aria-label")).toBe("Progress");
    expect(track.getAttribute("aria-valuenow")).toBe("40");
    const input = node.querySelector<HTMLInputElement>(".field-progressbar-input")!;
    input.value = "75";
    input.dispatchEvent(new Event("change"));
    expect(change).toHaveBeenLastCalledWith(75);
    expect(track.getAttribute("aria-valuenow")).toBe("75");
    input.value = "";
    input.dispatchEvent(new Event("change"));
    expect(change).toHaveBeenLastCalledWith(false);
    expect(track.getAttribute("aria-valuenow")).toBe("0");
  });
});
