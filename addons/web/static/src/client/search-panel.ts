/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Left-hand Search Panel sidebar for SectionsPanel metadata (`category` trees and `filter` checklists).
// Desktop (>= 1024px) displays a collapsible 232px rail; narrower / portrait viewports collapse into an
// expandable top bar drawer so the main view never overflows horizontally.
import { button, el } from "./dom.js";
import { icon, type IconName } from "./icons.js";
import type { AdmittedSection, SectionsPanel } from "./panel-metadata.js";
import { array, object } from "./metadata.js";
import { ClientError, type RpcClient } from "./transport.js";

export interface SearchPanelValue {
  id: number | string;
  label: string;
  count?: number;
  parentId?: number | string | false;
  groupId?: number | string | false;
  groupName?: string;
}

export interface SearchPanelSidebar {
  /** Wrap any view content container with the responsive sidebar + main-content layout. */
  readonly element: HTMLElement;
  /** Toggle button for collapsing/expanding the panel on narrow/portrait viewports or desktop. */
  readonly toggleButton: HTMLButtonElement;
  /** Active domain contributed by selected categories and filters. */
  getDomain(): unknown[];
  /** The selection as the address keeps it (`search_panel`). */
  getSelection(): string;
  /** Reload section values and counters against the current search domain. */
  refresh(searchDomain: readonly unknown[]): Promise<void>;
  dispose(): void;
}

export interface SearchPanelOptions {
  model: string;
  panel: SectionsPanel;
  rpc: RpcClient;
  context: Record<string, unknown>;
  /** The address's `search_panel` value: it replaces the sections' default selection. */
  selection?: string;
  /** Called whenever the user changes a category or filter selection, with the new `getSelection()`. */
  onChange: (selection: string) => void;
  /** Report non-fatal load failures to the shell without crashing the view. */
  report?: (error: unknown) => void;
}

const SECTION_ICON_MAP: Readonly<Record<string, IconName>> = {
  business: "building",
  building: "building",
  company: "building",
  group: "users",
  users: "users",
  department: "users",
  folder: "folder",
  filter_alt: "sliders-horizontal",
  tags: "tags",
  tag: "tag",
  briefcase: "briefcase",
  map: "map-pin",
  globe: "globe",
};

function resolveSectionIcon(token: string | null, kind: "category" | "filter"): IconName {
  if (token && Object.hasOwn(SECTION_ICON_MAP, token)) return SECTION_ICON_MAP[token]!;
  return kind === "category" ? "folder" : "sliders-horizontal";
}

function parseEntryId(raw: unknown): number | string | null {
  if (typeof raw === "number" && Number.isSafeInteger(raw)) return raw;
  if (typeof raw === "string" && raw.length > 0) return raw;
  return null;
}

function parseParentId(raw: unknown): number | string | false {
  if (raw === false || raw === null || raw === undefined) return false;
  if (Array.isArray(raw) && raw.length >= 1) return parseEntryId(raw[0]) ?? false;
  return parseEntryId(raw) ?? false;
}

function parseLabel(raw: unknown): string {
  if (typeof raw === "string") return raw;
  return "";
}

function parseRangeValues(raw: unknown): { parentField: string | false; values: SearchPanelValue[]; error?: string } {
  const payload = object(raw, "search panel range");
  if (typeof payload["error_msg"] === "string" && payload["error_msg"]) {
    return { parentField: false, values: [], error: payload["error_msg"] };
  }
  const parentField = typeof payload["parent_field"] === "string" && payload["parent_field"] ? payload["parent_field"] : false;
  const rows = array(payload["values"], "search panel values");
  const values: SearchPanelValue[] = [];
  for (const row of rows) {
    const item = object(row, "search panel value");
    const id = parseEntryId(item["id"]);
    if (id === null) continue;
    const count = typeof item["__count"] === "number" ? item["__count"] : undefined;
    const parentId = parentField ? parseParentId(item[parentField]) : false;
    const groupId = item["group_id"] !== undefined ? parseParentId(item["group_id"]) : undefined;
    const groupName = typeof item["group_name"] === "string" ? item["group_name"] : undefined;
    values.push({
      id,
      label: parseLabel(item["display_name"]) || String(id),
      ...(count !== undefined ? { count } : {}),
      ...(parentField ? { parentId } : {}),
      ...(groupId !== undefined ? { groupId } : {}),
      ...(groupName !== undefined ? { groupName } : {}),
    });
  }
  return { parentField, values };
}

/** Build the reusable left-hand Search Panel sidebar for any view matching `panel.viewTypes`. */
export function createSearchPanelSidebar(options: SearchPanelOptions): SearchPanelSidebar {
  const { model, panel, rpc, context, selection, report } = options;

  // Selection state: categories keep at most one active value; filters keep a set of checked values.
  const activeCategories = new Map<number, number | string>();
  const activeFilters = new Map<number, Set<number | string>>();
  const expandedNodes = new Map<number, Set<number | string>>();
  const sectionData = new Map<number, { parentField: string | false; values: SearchPanelValue[]; error?: string }>();

  for (const section of panel.sections) {
    if (section.kind === "category" && section.default_values.length === 1) {
      activeCategories.set(section.id, section.default_values[0]!);
    } else if (section.kind === "filter" && section.default_values.length > 0) {
      activeFilters.set(section.id, new Set(section.default_values));
    }
    expandedNodes.set(section.id, new Set());
  }
  if (selection !== undefined) {
    activeCategories.clear();
    activeFilters.clear();
    const refused = (): never => { throw new ClientError("validation", "Invalid navigation parameter: search_panel."); };
    // route.ts admitted the shape; what each section accepts is checked here.
    for (const [key, values] of Object.entries(JSON.parse(selection) as Record<string, (number | string)[]>)) {
      const section = panel.sections.find(candidate => candidate.id === Number(key));
      if (!section || (section.kind === "category" && values.length > 1)
          || values.some(value => typeof value !== (section.field.kind === "selection" ? "string" : "number"))) refused();
      if (!values.length) continue;
      if (section!.kind === "category") activeCategories.set(section!.id, values[0]!);
      else activeFilters.set(section!.id, new Set(values));
    }
  }
  const getSelection = (): string => {
    const kept: Record<string, (number | string)[]> = {};
    for (const [id, value] of activeCategories) kept[id] = [value];
    for (const [id, values] of activeFilters) if (values.size) kept[id] = [...values];
    return JSON.stringify(kept);
  };
  const onChange = (): void => options.onChange(getSelection());

  let collapsed = false;
  let mobileOpen = false;
  let disposed = false;
  let requestSeq = 0;

  const sidebar = el("aside", undefined, "search-panel");
  sidebar.setAttribute("aria-label", "Categories and filters");

  const headerBar = el("div", undefined, "search-panel-top");
  const mobileSummary = el("span", "Categories", "search-panel-mobile-label");
  const toggleButton = button("", () => {
    const narrow = typeof window !== "undefined" && window.innerWidth < 1024;
    if (narrow) {
      mobileOpen = !mobileOpen;
      sidebar.classList.toggle("is-mobile-open", mobileOpen);
      toggleButton.setAttribute("aria-expanded", String(mobileOpen));
    } else {
      collapsed = !collapsed;
      sidebar.classList.toggle("is-collapsed", collapsed);
      toggleButton.setAttribute("aria-expanded", String(!collapsed));
      toggleButton.title = collapsed ? "Show categories panel" : "Hide categories panel";
    }
  }, "search-panel-toggle");
  toggleButton.append(icon("panels-top-left"));
  toggleButton.setAttribute("aria-label", "Toggle categories panel");
  toggleButton.setAttribute("aria-expanded", "true");
  toggleButton.title = "Hide categories panel";

  const body = el("div", undefined, "search-panel-sections");
  headerBar.append(mobileSummary, toggleButton);
  sidebar.append(headerBar, body);

  const categoryDomain = (excludeId?: number): unknown[] => {
    const domain: unknown[] = [];
    for (const section of panel.sections) {
      if (section.kind !== "category" || section.id === excludeId) continue;
      const selected = activeCategories.get(section.id);
      if (selected === undefined) continue;
      const op = section.hierarchize && section.field.kind === "many2one" ? "child_of" : "=";
      domain.push([section.field.name, op, selected]);
    }
    return domain;
  };

  const filterDomain = (excludeId?: number): unknown[] => {
    const domain: unknown[] = [];
    for (const section of panel.sections) {
      if (section.kind !== "filter" || section.id === excludeId) continue;
      const selected = activeFilters.get(section.id);
      if (!selected || selected.size === 0) continue;
      domain.push([section.field.name, "in", [...selected]]);
    }
    return domain;
  };

  const getDomain = (): unknown[] => [...categoryDomain(), ...filterDomain()];

  const updateMobileBadge = (): void => {
    let activeCount = 0;
    for (const section of panel.sections) {
      if (section.kind === "category" && activeCategories.has(section.id)) activeCount++;
      if (section.kind === "filter") activeCount += activeFilters.get(section.id)?.size ?? 0;
    }
    mobileSummary.textContent = activeCount > 0 ? `Categories (${activeCount})` : "Categories";
  };

  const renderCategorySection = (section: Extract<AdmittedSection, { kind: "category" }>, host: HTMLElement): void => {
    const data = sectionData.get(section.id);
    if (data?.error) {
      host.append(el("p", data.error, "search-panel-note muted"));
      return;
    }
    const values = data?.values ?? [];
    const active = activeCategories.get(section.id);
    const expanded = expandedNodes.get(section.id)!;

    const list = el("ul", undefined, "search-panel-list");
    list.setAttribute("role", "tree");
    list.setAttribute("aria-label", section.label);

    // "All" root item
    const allLi = el("li", undefined, "search-panel-item");
    allLi.setAttribute("role", "treeitem");
    const allRow = el("div", undefined, `search-panel-row${active === undefined ? " is-active" : ""}`);
    if (section.hierarchize && data?.parentField) {
      allRow.append(el("span", undefined, "search-panel-caret-spacer"));
    }
    const allBtn = button("", () => {
      if (!activeCategories.has(section.id)) return;
      activeCategories.delete(section.id);
      updateMobileBadge();
      render();
      onChange();
    }, "search-panel-pick");
    allBtn.dataset["focusKey"] = `${section.id}:all`;
    allBtn.append(el("span", "All", "search-panel-label"));
    allBtn.setAttribute("aria-current", active === undefined ? "true" : "false");
    allRow.append(allBtn);
    allLi.append(allRow);
    list.append(allLi);

    // Build parent -> children map when hierarchize is active and parent_field exists.
    const byId = new Map<number | string, SearchPanelValue>();
    for (const item of values) byId.set(item.id, item);

    const childrenMap = new Map<number | string | "ROOT", SearchPanelValue[]>();
    const pushChild = (key: number | string | "ROOT", item: SearchPanelValue): void => {
      const bucket = childrenMap.get(key) ?? [];
      bucket.push(item);
      childrenMap.set(key, bucket);
    };

    if (section.hierarchize && data?.parentField) {
      for (const item of values) {
        const pid = item.parentId;
        if (pid !== undefined && pid !== false && byId.has(pid) && pid !== item.id) {
          pushChild(pid, item);
        } else {
          pushChild("ROOT", item);
        }
      }
      // Auto-expand root parents on initial load if there is a single top root (e.g. "Management").
      const roots = childrenMap.get("ROOT") ?? [];
      if (roots.length === 1 && expanded.size === 0 && (childrenMap.get(roots[0]!.id)?.length ?? 0) > 0) {
        expanded.add(roots[0]!.id);
      }
    } else {
      for (const item of values) pushChild("ROOT", item);
    }

    const renderBranch = (parentKey: number | string | "ROOT", container: HTMLElement, depth: number): void => {
      const items = childrenMap.get(parentKey) ?? [];
      for (const item of items) {
        const kids = childrenMap.get(item.id) ?? [];
        const hasKids = kids.length > 0;
        const isOpen = expanded.has(item.id);
        const isActive = active === item.id;

        const li = el("li", undefined, "search-panel-item");
        li.setAttribute("role", "treeitem");
        if (hasKids) li.setAttribute("aria-expanded", String(isOpen));

        const row = el("div", undefined, `search-panel-row${isActive ? " is-active" : ""}`);
        row.style.setProperty("--panel-depth", String(depth));

        if (hasKids) {
          const caret = button("", () => {
            if (expanded.has(item.id)) expanded.delete(item.id);
            else expanded.add(item.id);
            render();
          }, "search-panel-caret");
          caret.addEventListener("click", event => event.stopPropagation());
          caret.dataset["focusKey"] = `${section.id}:caret:${String(item.id)}`;
          caret.append(icon(isOpen ? "chevron-down" : "chevron-right"));
          caret.setAttribute("aria-label", `${isOpen ? "Collapse" : "Expand"} ${item.label}`);
          row.append(caret);
        } else if (section.hierarchize && data?.parentField) {
          row.append(el("span", undefined, "search-panel-caret-spacer"));
        }

        const pick = button("", () => {
          if (activeCategories.get(section.id) === item.id) activeCategories.delete(section.id);
          else activeCategories.set(section.id, item.id);
          if (hasKids && !expanded.has(item.id)) expanded.add(item.id);
          updateMobileBadge();
          render();
          onChange();
        }, "search-panel-pick");
        pick.setAttribute("aria-current", isActive ? "true" : "false");
        pick.dataset["focusKey"] = `${section.id}:pick:${String(item.id)}`;
        const displayLabel =
          section.hierarchize && data?.parentField && depth > 0 && item.label.includes(" / ")
            ? (item.label.split(" / ").at(-1) ?? item.label)
            : item.label;
        const lblSpan = el("span", displayLabel, "search-panel-label");
        if (displayLabel !== item.label) lblSpan.title = item.label;
        pick.append(lblSpan);
        if (section.enable_counters && item.count !== undefined) {
          pick.append(el("span", String(item.count), "search-panel-count"));
        }
        row.append(pick);
        li.append(row);

        if (hasKids && isOpen) {
          const sub = el("ul", undefined, "search-panel-sublist");
          sub.setAttribute("role", "group");
          renderBranch(item.id, sub, depth + 1);
          li.append(sub);
        }
        container.append(li);
      }
    };

    renderBranch("ROOT", list, 0);
    host.append(list);
  };

  const renderFilterSection = (section: Extract<AdmittedSection, { kind: "filter" }>, host: HTMLElement): void => {
    const data = sectionData.get(section.id);
    if (data?.error) {
      host.append(el("p", data.error, "search-panel-note muted"));
      return;
    }
    const values = data?.values ?? [];
    const selected = activeFilters.get(section.id) ?? new Set<number | string>();
    const list = el("ul", undefined, "search-panel-list");
    for (const item of values) {
      const li = el("li", undefined, "search-panel-item");
      const labelEl = el("label", undefined, `search-panel-check${selected.has(item.id) ? " is-active" : ""}`);
      const checkbox = el("input");
      checkbox.type = "checkbox";
      checkbox.checked = selected.has(item.id);
      checkbox.dataset["focusKey"] = `${section.id}:check:${String(item.id)}`;
      checkbox.addEventListener("change", () => {
        const next = activeFilters.get(section.id) ?? new Set<number | string>();
        if (checkbox.checked) next.add(item.id);
        else next.delete(item.id);
        activeFilters.set(section.id, next);
        updateMobileBadge();
        render();
        onChange();
      });
      labelEl.append(checkbox, el("span", item.label, "search-panel-label"));
      if (section.enable_counters && item.count !== undefined) {
        labelEl.append(el("span", String(item.count), "search-panel-count"));
      }
      li.append(labelEl);
      list.append(li);
    }
    host.append(list);
  };

  const render = (): void => {
    // A pick rebuilds the sections; the control that had focus gets it back, so keyboard users keep their place.
    const focused = document.activeElement instanceof HTMLElement && body.contains(document.activeElement)
      ? document.activeElement.dataset["focusKey"] : undefined;
    body.replaceChildren();
    updateMobileBadge();
    if (panel.notice) body.append(el("p", panel.notice, "search-panel-note muted"));
    for (const section of panel.sections) {
      const wrap = el("section", undefined, "search-panel-section");
      const heading = el("h2", undefined, "search-panel-heading");
      heading.append(icon(resolveSectionIcon(section.icon, section.kind)), el("span", section.label));
      wrap.append(heading);
      if (section.kind === "category") renderCategorySection(section, wrap);
      else renderFilterSection(section, wrap);
      body.append(wrap);
    }
    if (focused !== undefined) [...body.querySelectorAll<HTMLElement>("[data-focus-key]")].find(node => node.dataset["focusKey"] === focused)?.focus();
  };

  const refresh = async (searchDomain: readonly unknown[]): Promise<void> => {
    const seq = ++requestSeq;
    sidebar.setAttribute("aria-busy", "true");
    try {
      await Promise.all(panel.sections.map(async section => {
        const catDom = categoryDomain(section.kind === "category" ? section.id : undefined);
        const filDom = filterDomain(section.kind === "filter" ? section.id : undefined);
        if (section.kind === "category") {
          const raw = await rpc.call(model, "search_panel_select_range", {
            field_name: section.field.name,
            category_domain: catDom,
            filter_domain: filDom,
            search_domain: [...searchDomain],
            enable_counters: section.enable_counters,
            expand: section.expand,
            hierarchize: section.hierarchize,
            limit: section.limit || undefined,
            context,
          });
          if (disposed || seq !== requestSeq) return;
          sectionData.set(section.id, parseRangeValues(raw));
        } else {
          const raw = await rpc.call(model, "search_panel_select_multi_range", {
            field_name: section.field.name,
            category_domain: catDom,
            filter_domain: filDom,
            search_domain: [...searchDomain],
            enable_counters: section.enable_counters,
            expand: section.expand,
            group_by: section.group_by?.name,
            limit: section.limit || undefined,
            context,
          });
          if (disposed || seq !== requestSeq) return;
          sectionData.set(section.id, parseRangeValues(raw));
        }
      }));
    } catch (error) {
      if (!disposed && seq === requestSeq) {
        report?.(error);
      }
    } finally {
      if (!disposed && seq === requestSeq) {
        sidebar.removeAttribute("aria-busy");
        render();
      }
    }
  };

  render();
  return {
    element: sidebar,
    toggleButton,
    getDomain,
    getSelection,
    refresh,
    dispose: () => {
      disposed = true;
      requestSeq++;
      sidebar.remove();
    },
  };
}
