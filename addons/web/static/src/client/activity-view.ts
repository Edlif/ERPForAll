/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Activity Matrix view renderer: rows of records vs. mail.activity.type columns,
// color-coded overdue/today/planned deadline badges, quick mark-done, and inline scheduling.
import { button, el } from "./dom.js";
import { icon } from "./icons.js";
import { imageDataUrl } from "./image-field.js";
import { initials } from "./kanban.js";
import { ClientError, type RpcClient } from "./transport.js";
import type { ActivityNode, Field, Node, View } from "./metadata.js";
import type { DialogHandle, DialogService } from "./dialog-service.js";
import { t } from "./translate.js";

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
  /** The page's dialog service: schedule and detail dialogs trap focus, close on Escape and return focus. */
  dialogs: DialogService;
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

/** Today's date as `YYYY-MM-DD` in the browser's own time zone (not UTC, which is a different day near midnight). */
export function localDateIso(now = new Date()): string {
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

export function renderActivityMatrix(options: ActivityMatrixOptions): ActivityMatrixRendered {
  const { model, view, records, types, activities, rpc, context, userId, open, formatField, onChanged, report, dialogs } = options;
  const root = activityRoot(view);
  const fields = activitySummaryFields(root);
  const titleField = fields[0];
  const subFields = fields.slice(1);
  const avatarFieldName = root.avatarField?.name;

  // Group activities by `${res_id}:${activity_type_id}` and compute per-type column counts.
  const byCell = new Map<string, ActivityItemRow[]>();
  const colCounts = new Map<number, { overdue: number; today: number; planned: number }>();
  for (const type of types) colCounts.set(type.id, { overdue: 0, today: 0, planned: 0 });
  for (const act of activities) {
    const key = `${act.res_id}:${act.activity_type_id}`;
    const bucket = byCell.get(key) ?? [];
    bucket.push(act);
    byCell.set(key, bucket);
    const counts = colCounts.get(act.activity_type_id);
    if (counts) counts[act.state]++;
  }

  // The dialog service owns focus (trap, Escape, return to the opener); this keeps only the open handle.
  let activeDialog: DialogHandle | undefined;
  const closeDialog = (): void => { activeDialog?.close(); };
  const openDialog = (title: string, body: HTMLElement, footer: HTMLElement): DialogHandle => {
    closeDialog();
    const handle = dialogs.open({ title, body, footer, size: "sm", onClose: () => { if (activeDialog === handle) activeDialog = undefined; } });
    activeDialog = handle;
    return handle;
  };
  // A write succeeded, then the reload runs; a failed reload is reported like the write's own failure.
  const afterWrite = (write: Promise<unknown>, failed: () => void): void => {
    void write.then(() => { closeDialog(); return onChanged(); }, (error: unknown) => { failed(); report(error); }).catch(report);
  };

  const titleOf = (rec: Record<string, unknown>): string => {
    if (titleField) {
      const value = formatField(titleField, rec);
      if (value) return value;
    }
    const name = rec["display_name"] ?? rec["name"];
    return typeof name === "string" && name ? name : `#${String(rec["id"])}`;
  };

  /** Schedules an activity; without `recId` (the header command) the form asks for the record. */
  const openScheduleModal = (recId: number | undefined, defaultTypeId?: number): void => {
    const form = el("form", undefined, "activity-modal-form");
    const fieldRow = (label: string, control: HTMLElement): HTMLElement => {
      const row = el("label", undefined, "activity-modal-field");
      row.append(el("span", label), control);
      return row;
    };

    const recordSelect = el("select");
    recordSelect.name = "res_id";
    for (const rec of records) {
      if (typeof rec["id"] === "number") recordSelect.append(new Option(titleOf(rec), String(rec["id"])));
    }
    if (recId === undefined) form.append(fieldRow(t("Record"), recordSelect));

    const typeSelect = el("select");
    typeSelect.name = "activity_type_id";
    for (const type of types) {
      const opt = new Option(type.name, String(type.id));
      if (type.id === defaultTypeId) opt.selected = true;
      typeSelect.append(opt);
    }
    const summaryInput = el("input");
    summaryInput.type = "text";
    summaryInput.name = "summary";
    summaryInput.autofocus = true;
    const dateInput = el("input");
    dateInput.type = "date";
    dateInput.name = "date_deadline";
    dateInput.value = localDateIso();
    dateInput.required = true;
    form.append(fieldRow(t("Activity type"), typeSelect), fieldRow(t("Summary"), summaryInput), fieldRow(t("Due date"), dateInput));

    const footer = el("div", undefined, "actions");
    const cancelBtn = button(t("Cancel"), () => closeDialog());
    const submitBtn = button(t("Schedule"), () => form.requestSubmit(), "primary");
    submitBtn.dataset["defaultAction"] = "";
    footer.append(cancelBtn, submitBtn);

    form.addEventListener("submit", event => {
      event.preventDefault();
      const target = recId ?? Number(recordSelect.value);
      const typeId = Number(typeSelect.value);
      if (!Number.isSafeInteger(target) || target <= 0) { report(new ClientError("validation", t("Choose the record to schedule the activity for."))); return; }
      if (!Number.isSafeInteger(typeId) || typeId <= 0) { report(new ClientError("validation", t("Choose an activity type."))); return; }
      submitBtn.disabled = true;
      afterWrite(rpc.call("mail.activity", "create", {
        vals_list: [{
          res_model: model,
          res_id: target,
          activity_type_id: typeId,
          summary: summaryInput.value.trim() || null,
          date_deadline: dateInput.value || localDateIso(),
          user_id: userId,
        }],
        context,
      }, { write: true }), () => { submitBtn.disabled = false; });
    });

    const recTitle = recId === undefined ? undefined : titleOf(records.find(rec => rec["id"] === recId) ?? { id: recId });
    openDialog(recTitle === undefined ? t("Schedule activity") : t("Schedule activity for {record}", { record: recTitle }), form, footer);
  };

  const openCellDetail = (recId: number, recTitle: string, typeRow: ActivityTypeRow, items: readonly ActivityItemRow[]): void => {
    const list = el("div", undefined, "activity-detail-list");
    for (const item of items) {
      const row = el("div", undefined, `activity-detail-row is-${item.state}`);
      const info = el("div", undefined, "activity-detail-info");
      info.append(
        el("strong", item.summary || typeRow.name),
        el("span", `${item.date_deadline}${item.user_name ? ` · ${item.user_name}` : ""}`, "muted"),
      );
      const doneBtn = button(t("Mark done"), () => {
        doneBtn.disabled = true;
        afterWrite(rpc.call("mail.activity", "action_feedback", {
          ids: [item.id],
          feedback: null,
          attachment_ids: [],
          context,
        }, { write: true }), () => { doneBtn.disabled = false; });
      }, "secondary");
      row.append(info, doneBtn);
      list.append(row);
    }
    const footer = el("div", undefined, "actions");
    const another = button(t("Schedule another"), () => openScheduleModal(recId, typeRow.id), "primary");
    another.dataset["defaultAction"] = "";
    footer.append(button(t("Close"), () => closeDialog()), another);
    openDialog(t("{type} for {record}", { type: typeRow.name, record: recTitle }), list, footer);
  };

  const wrapper = el("div", undefined, "activity-matrix-wrap");
  const table = el("table", undefined, "activity-matrix");
  const thead = el("thead");
  const headRow = el("tr");

  const leadTh = el("th", undefined, "activity-matrix-lead-head");
  if (records.length > 0 && types.length > 0) {
    const schedBtn = button("", () => openScheduleModal(undefined, types[0]?.id), "activity-matrix-schedule-btn");
    schedBtn.append(icon("plus"), el("span", t("Schedule activity")));
    leadTh.append(schedBtn);
  } else {
    leadTh.textContent = t("Record");
  }
  headRow.append(leadTh);

  for (const type of types) {
    const th = el("th", undefined, "activity-matrix-type-head");
    th.append(el("span", type.name, "activity-type-title"));
    const counts = colCounts.get(type.id)!;
    const total = counts.overdue + counts.today + counts.planned;
    if (total > 0) {
      const bar = el("div", undefined, "activity-type-bar");
      bar.setAttribute("role", "img");
      bar.setAttribute("aria-label", t("{overdue} overdue, {today} due today, {planned} planned", counts));
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
    const emptyTd = el("td", t("No matching records to schedule activities for."), "activity-matrix-empty");
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

    for (const type of types) {
      const td = el("td", undefined, "activity-matrix-cell");
      const items = byCell.get(`${recId}:${type.id}`) ?? [];
      if (items.length > 0) {
        const stateOrder = { overdue: 0, today: 1, planned: 2 };
        const sorted = [...items].sort((a, b) => stateOrder[a.state] - stateOrder[b.state] || a.date_deadline.localeCompare(b.date_deadline));
        const top = sorted[0]!;
        const pill = button("", () => openCellDetail(recId, recTitle, type, sorted), `activity-cell-pill is-${top.state}`);
        pill.append(
          el("span", top.date_deadline, "activity-cell-date"),
          ...(items.length > 1 ? [el("span", `+${items.length - 1}`, "activity-cell-more")] : []),
        );
        pill.title = top.summary || type.name;
        td.append(pill);
      } else {
        const addBtn = button("", () => openScheduleModal(recId, type.id), "activity-cell-add");
        addBtn.append(icon("plus"));
        addBtn.setAttribute("aria-label", t("Schedule {type} for {record}", { type: type.name, record: recTitle }));
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
      closeDialog();
      wrapper.remove();
    },
  };
}
