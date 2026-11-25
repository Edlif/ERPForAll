/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { describe, expect, it, vi } from "vitest";
import { createSearchPanelSidebar } from "../src/client/search-panel.js";
import { renderHierarchyTree } from "../src/client/hierarchy.js";
import { renderActivityMatrix } from "../src/client/activity-view.js";
import { kanbanLayout, renderKanbanCards } from "../src/client/kanban.js";
import type { Field, View } from "../src/client/metadata.js";
import type { SectionsPanel } from "../src/client/panel-metadata.js";
import type { RpcClient } from "../src/client/rpc.js";

function field(name: string, kind: Field["kind"], label = name, extra: Partial<Field> = {}): Field {
  return { name, kind, label, required: false, readonly: false, ...extra };
}

describe("SearchPanelSidebar", () => {
  it("loads hierarchical category counters and emits child_of domains on click", async () => {
    const rpc = {
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
    } as unknown as RpcClient;

    const panel: SectionsPanel = {
      kind: "sections",
      version: 1,
      viewTypes: ["kanban", "list", "hierarchy"],
      sections: [{
        kind: "category",
        id: "department_id",
        field: field("department_id", "many2one", "Department", { relation: "hr.department" }),
        label: "Department",
        icon: "users",
        color: null,
        enable_counters: true,
        expand: true,
        hierarchize: true,
        limit: 200,
        default_values: [],
      }],
    };

    const onChange = vi.fn();
    const sidebar = createSearchPanelSidebar({
      model: "hr.employee",
      panel,
      rpc,
      context: {},
      onChange,
      report: () => {},
    });

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
});

describe("Hierarchy (Org Chart) renderer", () => {
  it("renders roots, expands children on toggle click, and invokes open callback", () => {
    const nameField = field("name", "char", "Name");
    const jobField = field("job_title", "char", "Job Title");
    const parentField = field("parent_id", "many2one", "Manager", { relation: "hr.employee" });
    const colorField = field("department_color", "integer", "Color");

    const view: View = {
      type: "hierarchy",
      fields: [nameField, jobField, parentField, colorField],
      nodes: [{
        kind: "hierarchy",
        parentField,
        colorField,
        draggable: true,
        label: "people",
        children: [{ kind: "field", field: nameField }, { kind: "field", field: jobField }],
      }],
    };

    const opened: number[] = [];
    const tree = renderHierarchyTree({
      view,
      records: [
        { id: 1, name: "Nadia Kusuma", job_title: "CEO", parent_id: false, department_color: 5 },
        { id: 2, name: "Budi Santoso", job_title: "VP Engineering", parent_id: { id: 1, display_name: "Nadia Kusuma" }, department_color: 4 },
        { id: 3, name: "Fajar Nugroho", job_title: "Principal Rust", parent_id: { id: 2, display_name: "Budi Santoso" }, department_color: 7 },
      ],
      open: id => opened.push(id),
      formatField: (f, rec) => String(rec[f.name] ?? ""),
    });

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
});

describe("Activity Matrix renderer", () => {
  it("renders record rows, activity type columns, and colored deadline states", () => {
    const nameField = field("name", "char", "Name");
    const view: View = {
      type: "activity",
      fields: [nameField],
      nodes: [{ kind: "activity", children: [{ kind: "field", field: nameField }] }],
    };
    const rpc = { call: vi.fn() } as unknown as RpcClient;
    const matrix = renderActivityMatrix({
      model: "hr.employee",
      view,
      records: [{ id: 10, name: "Nadia Kusuma" }],
      types: [{ id: 1, name: "Call" }, { id: 2, name: "Meeting" }],
      activities: [{
        id: 101,
        res_id: 10,
        activity_type_id: 1,
        summary: "Quarterly review",
        date_deadline: "2026-11-24",
        state: "overdue",
        user_name: "Admin",
      }],
      rpc,
      context: {},
      userId: 2,
      formatField: (f, rec) => String(rec[f.name] ?? ""),
      onChanged: async () => {},
      report: () => {},
    });

    expect(matrix.node.textContent).toContain("Nadia Kusuma");
    expect(matrix.node.textContent).toContain("Call");
    expect(matrix.node.textContent).toContain("Meeting");
    expect(matrix.node.querySelector(".activity-cell-pill.is-overdue")).not.toBeNull();
    matrix.dispose();
  });
});

describe("Rich portrait Kanban cards", () => {
  it("renders initials tile when image aside is empty and keeps media layout", () => {
    const imgField = field("image_128", "image", "Photo", { widget: "image" });
    const nameField = field("name", "char", "Name");
    const emailField = field("work_email", "char", "Email", { widget: "email" });
    const view: View = {
      type: "kanban",
      fields: [imgField, nameField, emailField],
      nodes: [{
        kind: "kanban",
        children: [{
          kind: "kanban_card",
          children: [
            { kind: "kanban_section", region: "aside", children: [{ kind: "field", field: imgField }] },
            { kind: "kanban_section", region: "main", children: [{ kind: "field", field: nameField }, { kind: "field", field: emailField }] },
            { kind: "kanban_section", region: "footer", children: [] },
          ],
        }],
      }],
    };
    const layout = kanbanLayout(view);
    const rendered = renderKanbanCards({
      layout,
      records: [{ id: 7, name: "Kartika Dewi", work_email: "kartika@example.id", image_128: false }],
      renderField: (f, rec) => {
        const span = document.createElement("span");
        span.textContent = String(rec[f.name] ?? "");
        return { node: span, dispose: () => {} };
      },
    });
    expect(rendered.node.querySelector(".kanban-card-has-media")).not.toBeNull();
    expect(rendered.node.querySelector(".kanban-card-media-initials")?.textContent).toBe("KD");
    rendered.dispose();
  });
});
