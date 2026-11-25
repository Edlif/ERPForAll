/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Activity Matrix view renderer: rows of records vs. mail.activity.type columns,
// color-coded overdue/today/planned deadline badges, quick mark-done, and inline scheduling.
import { button, el } from "./dom.js";
import { icon } from "./icons.js";
import { imageDataUrl } from "./image-field.js";
import { initials } from "./kanban.js";
import { ClientError, type RpcClient } from "./transport.js";
import type { ActivityNode, Field, Node, View } from "./metadata.js";

export interface ActivityTypeRow {
  id: number;
  name: string;
}

export interface ActivityItemRow {
  id: number;
  res_id: number;
  activity_type_id: number;
  summary: string;
  date_deadline: string;
  state: "overdue" | "today" | "planned";
  user_name: string;
}

export interface ActivityMatrixRendered {
  node: HTMLElement;
  dispose: () => void;
}

export interface ActivityMatrixOptions {
  model: string;
  view: View;
  records: readonly Record<string, unknown>[];
  types: readonly ActivityTypeRow[];
  activities: readonly ActivityItemRow[];
  rpc: RpcClient;
  context: Record<string, unknown>;
  userId: number;
  open?: (id: number) => void;
  formatField: (field: Field, record: Record<string, unknown>) => string;
  onChanged: () => Promise<void>;
  report: (error: unknown) => void;
}

export function activityRoot(view: View): ActivityNode {
  const root = view.nodes[0];
  if (view.type !== "activity" || root?.kind !== "activity") {
    throw new ClientError("server", "Invalid activity view description.");
  }
  return root;
}

export function activitySummaryFields(root: ActivityNode): Field[] {
  return root.children.flatMap((child: Node) =>
    child.kind === "field" && !child.field.invisible ? [child.field] : [],
  );
}

function todayIso(): string {
  const now = new Date();
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

export function renderActivityMatrix(options: ActivityMatrixOptions): ActivityMatrixRendered {
  const { model, view, records, types, activities, rpc, context, userId, open, formatField, onChanged, report } = options;
  const root = activityRoot(view);
  const fields = activitySummaryFields(root);
  const titleField = fields[0];
  const subFields = fields.slice(1);
  const avatarFieldName = root.avatarField?.name;

  // Group activities by `${res_id}:${activity_type_id}` and compute per-type column counts.
  const byCell = new Map<string, ActivityItemRow[]>();
  const colCounts = new Map<number, { overdue: number; today: number; planned: number }>();
  for (const t of types) colCounts.set(t.id, { overdue: 0, today: 0, planned: 0 });
  for (const act of activities) {
    const key = `${act.res_id}:${act.activity_type_id}`;
    const bucket = byCell.get(key) ?? [];
    bucket.push(act);
    byCell.set(key, bucket);
    const counts = colCounts.get(act.activity_type_id);
    if (counts) counts[act.state]++;
  }

  let activePopover: HTMLElement | undefined;
  const closePopover = (): void => {
    activePopover?.remove();
    activePopover = undefined;
  };

  const titleOf = (rec: Record<string, unknown>): string => {
    if (titleField) {
      const value = formatField(titleField, rec);
      if (value) return value;
    }
    const name = rec["display_name"] ?? rec["name"];
    return typeof name === "string" && name ? name : `#${String(rec["id"])}`;
  };

  const openScheduleModal = (recId: number, recTitle: string, defaultTypeId?: number): void => {
    closePopover();
    const backdrop = el("div", undefined, "activity-modal-backdrop");
    const dialog = el("div", undefined, "activity-modal");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    dialog.setAttribute("aria-label", `Schedule activity for ${recTitle}`);

    const heading = el("h3", `Schedule Activity — ${recTitle}`, "activity-modal-title");
    const form = el("form", undefined, "activity-modal-form");

    const typeSelect = el("select");
    typeSelect.name = "activity_type_id";
    for (const t of types) {
      const opt = new Option(t.name, String(t.id));
      if (t.id === defaultTypeId) opt.selected = true;
      typeSelect.append(opt);
    }
    const typeLabel = el("label", undefined, "activity-modal-field");
    typeLabel.append(el("span", "Activity Type"), typeSelect);

    const summaryInput = el("input");
    summaryInput.type = "text";
    summaryInput.name = "summary";
    summaryInput.placeholder = "e.g. Onboarding check-in, Contract review…";
    const summaryLabel = el("label", undefined, "activity-modal-field");
    summaryLabel.append(el("span", "Summary"), summaryInput);

    const dateInput = el("input");
    dateInput.type = "date";
    dateInput.name = "date_deadline";
    dateInput.value = todayIso();
    dateInput.required = true;
    const dateLabel = el("label", undefined, "activity-modal-field");
    dateLabel.append(el("span", "Due Date"), dateInput);

    const actions = el("div", undefined, "activity-modal-actions");
    const submitBtn = button("Schedule", () => {}, "primary");
    submitBtn.type = "submit";
    const cancelBtn = button("Cancel", () => backdrop.remove(), "secondary");
    actions.append(submitBtn, cancelBtn);

    form.append(typeLabel, summaryLabel, dateLabel, actions);
    form.addEventListener("submit", event => {
      event.preventDefault();
      const typeId = Number(typeSelect.value);
      if (!Number.isSafeInteger(typeId) || typeId <= 0) return;
      submitBtn.disabled = true;
      void rpc.call("mail.activity", "create", {
        vals_list: [{
          res_model: model,
          res_id: recId,
          activity_type_id: typeId,
          summary: summaryInput.value.trim() || null,
          date_deadline: dateInput.value || todayIso(),
          user_id: userId,
        }],
        context,
      }, { write: true }).then(async () => {
        backdrop.remove();
        await onChanged();
      }, error => {
        submitBtn.disabled = false;
        report(error);
      });
    });

    backdrop.addEventListener("click", event => {
      if (event.target === backdrop) backdrop.remove();
    });
    dialog.append(heading, form);
    backdrop.append(dialog);
    document.body.append(backdrop);
    activePopover = backdrop;
    summaryInput.focus();
  };

  const openCellDetail = (recId: number, recTitle: string, typeRow: ActivityTypeRow, items: readonly ActivityItemRow[]): void => {
    closePopover();
    const backdrop = el("div", undefined, "activity-modal-backdrop");
    const dialog = el("div", undefined, "activity-modal");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    dialog.setAttribute("aria-label", `${typeRow.name} for ${recTitle}`);

    dialog.append(el("h3", `${typeRow.name} — ${recTitle}`, "activity-modal-title"));
    const list = el("div", undefined, "activity-detail-list");
    for (const item of items) {
      const row = el("div", undefined, `activity-detail-row is-${item.state}`);
      const info = el("div", undefined, "activity-detail-info");
      info.append(
        el("strong", item.summary || typeRow.name),
        el("span", `${item.date_deadline}${item.user_name ? ` · ${item.user_name}` : ""}`, "muted"),
      );
      const doneBtn = button("Mark Done", () => {
        doneBtn.disabled = true;
        void rpc.call("mail.activity", "action_feedback", {
          ids: [item.id],
          feedback: null,
          attachment_ids: [],
          context,
        }, { write: true }).then(async () => {
          backdrop.remove();
          await onChanged();
        }, error => {
          doneBtn.disabled = false;
          report(error);
        });
      }, "secondary");
      row.append(info, doneBtn);
      list.append(row);
    }
    const actions = el("div", undefined, "activity-modal-actions");
    actions.append(
      button("+ Schedule Another", () => openScheduleModal(recId, recTitle, typeRow.id), "primary"),
      button("Close", () => backdrop.remove(), "secondary"),
    );
    backdrop.addEventListener("click", event => {
      if (event.target === backdrop) backdrop.remove();
    });
    dialog.append(list, actions);
    backdrop.append(dialog);
    document.body.append(backdrop);
    activePopover = backdrop;
  };

  const wrapper = el("div", undefined, "activity-matrix-wrap");
  const table = el("table", undefined, "activity-matrix");
  const thead = el("thead");
  const headRow = el("tr");

  const leadTh = el("th", undefined, "activity-matrix-lead-head");
  if (records.length > 0 && types.length > 0) {
    const firstRec = records[0]!;
    const firstId = Number(firstRec["id"]);
    const schedBtn = button("", () => openScheduleModal(firstId, titleOf(firstRec), types[0]?.id), "activity-matrix-schedule-btn");
    schedBtn.append(icon("plus"), el("span", "Schedule activity"));
    leadTh.append(schedBtn);
  } else {
    leadTh.textContent = "Record";
  }
  headRow.append(leadTh);

  for (const t of types) {
    const th = el("th", undefined, "activity-matrix-type-head");
    th.append(el("span", t.name, "activity-type-title"));
    const counts = colCounts.get(t.id)!;
    const total = counts.overdue + counts.today + counts.planned;
    if (total > 0) {
      const bar = el("div", undefined, "activity-type-bar");
      if (counts.overdue) {
        const seg = el("span", undefined, "activity-bar-overdue");
        seg.style.flex = String(counts.overdue);
        bar.append(seg);
      }
      if (counts.today) {
        const seg = el("span", undefined, "activity-bar-today");
        seg.style.flex = String(counts.today);
        bar.append(seg);
      }
      if (counts.planned) {
        const seg = el("span", undefined, "activity-bar-planned");
        seg.style.flex = String(counts.planned);
        bar.append(seg);
      }
      th.append(bar);
    }
    headRow.append(th);
  }
  thead.append(headRow);
  table.append(thead);

  const tbody = el("tbody");
  if (records.length === 0) {
    const emptyTr = el("tr");
    const emptyTd = el("td", "No matching records to schedule activities for.", "activity-matrix-empty");
    emptyTd.colSpan = Math.max(1, types.length + 1);
    emptyTr.append(emptyTd);
    tbody.append(emptyTr);
  }
  for (const rec of records) {
    const recId = rec["id"];
    if (typeof recId !== "number") continue;
    const recTitle = titleOf(rec);
    const tr = el("tr");

    const leadTd = el("td", undefined, "activity-matrix-lead");
    const recWrap = el("div", undefined, "activity-record-cell");
    const avatar = el("div", undefined, "activity-record-avatar");
    const rawImg = avatarFieldName ? rec[avatarFieldName] : undefined;
    if (typeof rawImg === "string" && rawImg.length > 0 && /^[A-Za-z0-9+/]+=*$/.test(rawImg)) {
      const img = el("img");
      img.src = imageDataUrl(rawImg);
      img.alt = recTitle;
      img.loading = "lazy";
      avatar.append(img);
    } else {
      avatar.textContent = initials(recTitle) || "?";
    }
    const info = el("div", undefined, "activity-record-info");
    if (open) {
      info.append(button(recTitle, () => open(recId), "activity-record-link link-button"));
    } else {
      info.append(el("span", recTitle, "activity-record-name"));
    }
    for (const sub of subFields) {
      if (sub.name === avatarFieldName) continue;
      const value = formatField(sub, rec);
      if (value) info.append(el("span", value, "activity-record-sub muted"));
    }
    recWrap.append(avatar, info);
    leadTd.append(recWrap);
    tr.append(leadTd);

    for (const t of types) {
      const td = el("td", undefined, "activity-matrix-cell");
      const items = byCell.get(`${recId}:${t.id}`) ?? [];
      if (items.length > 0) {
        const stateOrder = { overdue: 0, today: 1, planned: 2 };
        const sorted = [...items].sort((a, b) => stateOrder[a.state] - stateOrder[b.state] || a.date_deadline.localeCompare(b.date_deadline));
        const top = sorted[0]!;
        const pill = button("", () => openCellDetail(recId, recTitle, t, sorted), `activity-cell-pill is-${top.state}`);
        pill.append(
          el("span", top.date_deadline, "activity-cell-date"),
          ...(items.length > 1 ? [el("span", `+${items.length - 1}`, "activity-cell-more")] : []),
        );
        pill.title = top.summary || t.name;
        td.append(pill);
      } else {
        const addBtn = button("", () => openScheduleModal(recId, recTitle, t.id), "activity-cell-add");
        addBtn.append(icon("plus"));
        addBtn.setAttribute("aria-label", `Schedule ${t.name} for ${recTitle}`);
        td.append(addBtn);
      }
      tr.append(td);
    }
    tbody.append(tr);
  }
  table.append(tbody);
  wrapper.append(table);

  return {
    node: wrapper,
    dispose: () => {
      closePopover();
      wrapper.remove();
    },
  };
}
