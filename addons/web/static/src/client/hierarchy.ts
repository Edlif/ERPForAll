/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Hierarchy (Org Chart / Tree) view renderer.
// Renders top-level parent cards with pastel department-color headers, overlapping circular avatars,
// fold/unfold subordinate pills with bracket connectors, ancestor navigation chips, and optional
// drag-and-drop reassignment of `parent_field`.
import { button, el } from "./dom.js";
import { icon } from "./icons.js";
import { imageDataUrl } from "./image-field.js";
import { initials } from "./kanban.js";
import { ClientError } from "./transport.js";
import type { Field, HierarchyNode, Node, View } from "./metadata.js";
import type { DialogService } from "./dialog-service.js";
import { t } from "./translate.js";

export interface HierarchyRendered {
  node: HTMLElement;
  dispose: () => void;
}

/** What the user unfolded and focused; the view keeps one across reloads so a reassignment leaves the chart as it was. */
export interface HierarchyState { expanded?: Set<number>; focus?: number | null }

export interface HierarchyRenderOptions {
  view: View;
  records: readonly Record<string, unknown>[];
  /** Record id to focus initially (e.g. from "Full Chart" action context). */
  focusId?: number;
  /** Open the record's form view. */
  open?: (id: number) => void;
  /** Reassign a record's parent when `draggable` is true. */
  onReassign?: (childId: number, newParentId: number) => Promise<void>;
  /** Render a field read-only when custom display is needed. */
  formatField: (field: Field, record: Record<string, unknown>) => string;
  /** Kept across renders of one view; empty on the first render. */
  state?: HierarchyState;
  /** Opens the keyboard path for a reassignment (a choice of the new parent); without it only drag and drop moves a card. */
  dialogs?: DialogService;
}

export function hierarchyRoot(view: View): HierarchyNode {
  const root = view.nodes[0];
  if (view.type !== "hierarchy" || root?.kind !== "hierarchy") {
    throw new ClientError("server", "Invalid hierarchy view description.");
  }
  return root;
}

export function hierarchyCardFields(root: HierarchyNode): Field[] {
  return root.children.flatMap((child: Node) =>
    child.kind === "field" && !child.field.invisible ? [child.field] : [],
  );
}

function relationId(value: unknown): number | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value > 0) return value;
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const id = (value as Record<string, unknown>)["id"];
    if (typeof id === "number" && Number.isSafeInteger(id) && id > 0) return id;
  }
  if (Array.isArray(value) && typeof value[0] === "number" && value[0] > 0) return value[0];
  return null;
}

function paletteIndex(raw: unknown, seedText: string): number {
  if (typeof raw === "number" && Number.isFinite(raw) && raw > 0) return Math.abs(Math.trunc(raw)) % 12;
  let hash = 0;
  for (const ch of seedText) hash = (hash * 31 + ch.codePointAt(0)!) >>> 0;
  return (hash % 11) + 1;
}

/** Build the interactive org-chart / hierarchy tree canvas. */
export function renderHierarchyTree(options: HierarchyRenderOptions): HierarchyRendered {
  const { view, records, focusId, open, onReassign, formatField, dialogs } = options;
  const state = options.state ?? {};
  const rootNode = hierarchyRoot(view);
  const fields = hierarchyCardFields(rootNode);
  const titleField = fields[0];
  const subtitleFields = fields.slice(1);
  const parentFieldName = rootNode.parentField.name;
  const avatarFieldName = rootNode.avatarField?.name;
  const colorFieldName = rootNode.colorField?.name;
  const countFieldName = rootNode.countField?.name;
  const childNoun = rootNode.label ?? t("items");

  const byId = new Map<number, Record<string, unknown>>();
  const childrenMap = new Map<number, number[]>();
  const parentMap = new Map<number, number | null>();

  for (const rec of records) {
    const id = rec["id"];
    if (typeof id !== "number") continue;
    byId.set(id, rec);
  }
  for (const [id, rec] of byId) {
    const pid = relationId(rec[parentFieldName]);
    const validParent = pid !== null && pid !== id && byId.has(pid) ? pid : null;
    parentMap.set(id, validParent);
    if (validParent !== null) {
      const list = childrenMap.get(validParent) ?? [];
      list.push(id);
      childrenMap.set(validParent, list);
    }
  }

  // Compute total descendant count (avoiding cycles defensively).
  const descendantCount = (id: number, visited = new Set<number>()): number => {
    if (visited.has(id)) return 0;
    visited.add(id);
    const direct = childrenMap.get(id) ?? [];
    let total = direct.length;
    for (const childId of direct) total += descendantCount(childId, visited);
    return total;
  };

  const roots: number[] = [];
  for (const [id] of byId) {
    if (parentMap.get(id) === null) roots.push(id);
  }

  // Focused root: either explicit focusId or the single top-level root when there is only one; a later render of the
  // same view keeps what the user unfolded and focused.
  const restored = state.expanded !== undefined;
  const expanded = state.expanded ?? new Set<number>();
  state.expanded = expanded;
  let activeFocus: number | null = restored ? (state.focus !== undefined && state.focus !== null && byId.has(state.focus) ? state.focus : null)
    : focusId !== undefined && byId.has(focusId) ? focusId : null;
  if (restored) {
    for (const id of [...expanded]) if (!byId.has(id)) expanded.delete(id);
  } else if (activeFocus !== null) {
    // Expand the focused node (or its parent if leaf).
    if ((childrenMap.get(activeFocus)?.length ?? 0) > 0) {
      expanded.add(activeFocus);
    } else if (parentMap.get(activeFocus) !== null) {
      activeFocus = parentMap.get(activeFocus)!;
      expanded.add(activeFocus);
    }
  } else if (roots.length === 1 && (childrenMap.get(roots[0]!)?.length ?? 0) > 0) {
    expanded.add(roots[0]!);
  }

  const host = el("div", undefined, "hierarchy-canvas");
  host.setAttribute("role", "tree");
  host.setAttribute("aria-label", rootNode.parentField.label);
  let draggedId: number | null = null;
  let disposed = false;

  const titleOf = (rec: Record<string, unknown>): string => {
    if (titleField) {
      const text = formatField(titleField, rec);
      if (text) return text;
    }
    const name = rec["display_name"] ?? rec["name"];
    return typeof name === "string" && name ? name : `#${String(rec["id"])}`;
  };

  const avatarNode = (rec: Record<string, unknown>, title: string, colorIdx: number, small = false): HTMLElement => {
    const wrap = el("div", undefined, `hierarchy-avatar${small ? " hierarchy-avatar-sm" : ""} hierarchy-color-${colorIdx}`);
    const raw = avatarFieldName ? rec[avatarFieldName] : undefined;
    if (typeof raw === "string" && raw.length > 0 && /^[A-Za-z0-9+/]+=*$/.test(raw)) {
      const img = el("img");
      img.src = imageDataUrl(raw);
      img.alt = title;
      img.loading = "lazy";
      wrap.append(img);
    } else {
      wrap.textContent = initials(title) || "?";
    }
    return wrap;
  };

  const descendants = (id: number, found = new Set<number>()): Set<number> => {
    for (const child of childrenMap.get(id) ?? []) if (!found.has(child)) { found.add(child); descendants(child, found); }
    return found;
  };

  /** The keyboard path of a reassignment: choose the new parent among the loaded records (not the card or its subtree). */
  const changeParent = (id: number, title: string): void => {
    if (!dialogs || !onReassign) return;
    const excluded = descendants(id).add(id);
    const select = el("select");
    select.name = parentFieldName;
    select.autofocus = true;
    for (const [candidate, rec] of byId) {
      if (excluded.has(candidate)) continue;
      const option = new Option(titleOf(rec), String(candidate));
      option.selected = parentMap.get(id) === candidate;
      select.append(option);
    }
    const label = el("label", undefined, "activity-modal-field");
    label.append(el("span", rootNode.parentField.label), select);
    const footer = el("div", undefined, "actions");
    const save = button(t("Save"), () => {
      const target = Number(select.value);
      handle.close();
      if (Number.isSafeInteger(target) && target > 0 && target !== parentMap.get(id)) void onReassign(id, target);
    }, "primary");
    save.dataset["defaultAction"] = "";
    save.disabled = select.options.length === 0;
    footer.append(button(t("Cancel"), () => handle.close()), save);
    const handle = dialogs.open({ title: t("Change {field} of {record}", { field: rootNode.parentField.label, record: title }), body: label, footer, size: "sm" });
  };

  const buildCard = (id: number, isExpanded: boolean): HTMLElement => {
    const rec = byId.get(id)!;
    const title = titleOf(rec);
    const colorIdx = paletteIndex(colorFieldName ? rec[colorFieldName] : undefined, title);
    const directChildren = childrenMap.get(id) ?? [];
    const totalSub = descendantCount(id);

    const card = el("article", undefined, `hierarchy-card${open ? " hierarchy-card-clickable" : ""}`);
    card.dataset["id"] = String(id);
    card.setAttribute("role", "treeitem");
    card.setAttribute("aria-label", title);
    if (directChildren.length > 0) card.setAttribute("aria-expanded", String(isExpanded));
    if (open) {
      card.tabIndex = 0;
      card.addEventListener("keydown", event => {
        if (event.target !== card || (event.key !== "Enter" && event.key !== " ")) return;
        event.preventDefault();
        open(id);
      });
    }

    if (rootNode.draggable && onReassign) {
      card.draggable = true;
      card.addEventListener("dragstart", event => {
        draggedId = id;
        card.classList.add("is-dragging");
        event.dataTransfer?.setData("text/plain", String(id));
      });
      card.addEventListener("dragend", () => {
        draggedId = null;
        card.classList.remove("is-dragging");
        host.querySelectorAll(".is-drop-target").forEach(node => node.classList.remove("is-drop-target"));
      });
      card.addEventListener("dragover", event => {
        if (draggedId === null || draggedId === id) return;
        event.preventDefault();
        card.classList.add("is-drop-target");
      });
      card.addEventListener("dragleave", () => card.classList.remove("is-drop-target"));
      card.addEventListener("drop", event => {
        event.preventDefault();
        card.classList.remove("is-drop-target");
        const sourceId = draggedId;
        draggedId = null;
        if (sourceId !== null && sourceId !== id) {
          void onReassign(sourceId, id);
        }
      });
    }

    const header = el("div", undefined, `hierarchy-card-header hierarchy-color-${colorIdx}`);
    if (avatarFieldName) {
      header.append(avatarNode(rec, title, colorIdx, false));
    } else {
      header.classList.add("hierarchy-card-header-text");
      header.append(el("span", title, "hierarchy-card-header-title"));
    }
    card.append(header);

    const body = el("div", undefined, "hierarchy-card-body");
    if (avatarFieldName) {
      const nameEl = el("div", title, "hierarchy-card-name");
      body.append(nameEl);
    }
    for (const sub of subtitleFields) {
      if (sub.name === countFieldName || sub.name === colorFieldName || sub.name === avatarFieldName) continue;
      const text = formatField(sub, rec);
      if (text) body.append(el("div", text, "hierarchy-card-sub"));
    }
    if (countFieldName) {
      const val = rec[countFieldName];
      if (typeof val === "number") {
        body.append(el("div", t("{label}: {count}", { label: rootNode.countField!.label, count: val }), "hierarchy-card-stat"));
      }
    }
    card.append(body);

    if (open) {
      card.addEventListener("click", event => {
        if ((event.target as HTMLElement).closest("button")) return;
        open(id);
      });
    }

    if (directChildren.length > 0) {
      // The arch names one plural noun; "noun: count" reads right for any count without a plural rule per language.
      const toggleLabel = totalSub > directChildren.length
        ? t("{noun}: {count} ({total})", { noun: childNoun, count: directChildren.length, total: totalSub })
        : t("{noun}: {count}", { noun: childNoun, count: directChildren.length });
      const toggle = button("", () => {
        if (expanded.has(id)) {
          expanded.delete(id);
          if (activeFocus === id) activeFocus = parentMap.get(id) ?? null;
        } else {
          // Focus this node's branch cleanly so large org charts stay tidy.
          expanded.add(id);
          activeFocus = id;
        }
        render();
      }, `hierarchy-card-toggle${isExpanded ? " is-open" : ""}`);
      toggle.addEventListener("click", event => event.stopPropagation());
      toggle.append(icon(isExpanded ? "chevron-down" : "chevron-right"), el("span", toggleLabel));
      card.append(toggle);
    }

    if (rootNode.draggable && onReassign && dialogs) {
      const move = button("", () => changeParent(id, title), "hierarchy-card-move");
      move.addEventListener("click", event => event.stopPropagation());
      move.append(icon("network"));
      move.setAttribute("aria-label", t("Change {field} of {record}", { field: rootNode.parentField.label, record: title }));
      move.title = t("Change {field}", { field: rootNode.parentField.label });
      card.append(move);
    }

    return card;
  };

  const ancestorChain = (id: number): number[] => {
    const chain: number[] = [];
    const seen = new Set<number>();
    let current = parentMap.get(id) ?? null;
    while (current !== null && !seen.has(current)) {
      seen.add(current);
      chain.unshift(current);
      current = parentMap.get(current) ?? null;
    }
    return chain;
  };

  const renderLevel = (ids: readonly number[], container: HTMLElement): void => {
    const row = el("div", undefined, "hierarchy-row");
    row.setAttribute("role", "group");
    const openChildren: { parentId: number; children: number[] }[] = [];
    for (const id of ids) {
      const kids = childrenMap.get(id) ?? [];
      const isOpen = expanded.has(id) && kids.length > 0;
      row.append(buildCard(id, isOpen));
      if (isOpen) openChildren.push({ parentId: id, children: kids });
    }
    container.append(row);

    for (const branch of openChildren) {
      const parentRec = byId.get(branch.parentId)!;
      const parentTitle = titleOf(parentRec);
      const colorIdx = paletteIndex(colorFieldName ? parentRec[colorFieldName] : undefined, parentTitle);

      const branchWrap = el("div", undefined, "hierarchy-branch");
      const chipWrap = el("div", undefined, "hierarchy-manager-chip-wrap");
      const chip = button("", () => {
        if (open) open(branch.parentId);
      }, "hierarchy-manager-chip");
      if (avatarFieldName) chip.append(avatarNode(parentRec, parentTitle, colorIdx, true));
      chip.append(el("span", parentTitle));
      chipWrap.append(chip);
      branchWrap.append(chipWrap, el("div", undefined, "hierarchy-connector"));
      renderLevel(branch.children, branchWrap);
      container.append(branchWrap);
    }
  };

  const render = (): void => {
    if (disposed) return;
    state.focus = activeFocus;
    host.replaceChildren();
    if (byId.size === 0) return;

    // If focused deeper in the tree, show ancestor breadcrumb pills to jump back up.
    if (activeFocus !== null && parentMap.get(activeFocus) !== null) {
      const ancestors = ancestorChain(activeFocus);
      const trail = el("div", undefined, "hierarchy-ancestors");
      const allBtn = button(t("Top level"), () => {
        activeFocus = null;
        expanded.clear();
        if (roots.length === 1) expanded.add(roots[0]!);
        render();
      }, "hierarchy-ancestor-pill");
      trail.append(allBtn);
      for (const ancId of ancestors) {
        const rec = byId.get(ancId)!;
        const pill = button(titleOf(rec), () => {
          activeFocus = ancId;
          expanded.add(ancId);
          render();
        }, "hierarchy-ancestor-pill");
        trail.append(icon("chevron-right"), pill);
      }
      host.append(trail);
      renderLevel([activeFocus], host);
    } else {
      renderLevel(roots, host);
    }
  };

  render();
  return {
    node: host,
    dispose: () => {
      disposed = true;
      host.remove();
    },
  };
}
