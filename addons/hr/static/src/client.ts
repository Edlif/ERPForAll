/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { localDateIso, registerFieldWidget, type WidgetEnvironment, type WidgetHandle } from "@erpforall/client";

type Obj = Record<string, unknown>;

function asObj(value: unknown): Obj {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Unexpected response shape from HR service.");
  }
  return value as Obj;
}

function asList(value: unknown): Obj[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is Obj => item !== null && typeof item === "object" && !Array.isArray(item));
}

function asStr(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

function asNum(value: unknown, fallback = 0): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function m2oLabel(value: unknown, fallback = ""): string {
  if (value && typeof value === "object" && !Array.isArray(value)) {
    const label = (value as Obj)["display_name"];
    if (typeof label === "string" && label.trim()) return label;
  }
  return fallback;
}

function recordIdOf(env: WidgetEnvironment): number | null {
  const id = env.values?.["id"];
  return typeof id === "number" && Number.isSafeInteger(id) && id > 0 ? id : null;
}

function initialsOf(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "?";
  if (parts.length === 1) return parts[0]!.slice(0, 2).toUpperCase();
  return `${parts[0]![0] ?? ""}${parts[parts.length - 1]![0] ?? ""}`.toUpperCase();
}

function toneOf(seed: string): number {
  let hash = 0;
  for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) | 0;
  return Math.abs(hash) % 12;
}

function makeEl<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function makeButton(label: string, className: string, onClick: () => void): HTMLButtonElement {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = className;
  btn.textContent = label;
  btn.addEventListener("click", onClick);
  return btn;
}

/** Asks before removing a line; the widget's form provides the dialog service, and without it nothing is removed. */
async function confirmRemoval(env: WidgetEnvironment, title: string, message: string): Promise<boolean> {
  if (!env.messages) throw new Error(`${title}: this form offers no confirmation dialog, so nothing was removed.`);
  const decision = await env.messages.dialogs.confirm({ title, message, confirmLabel: "Remove", destructive: true }).result;
  return decision === "confirm";
}

function dataUriForImage(raw: unknown): string | null {
  if (typeof raw !== "string" || !raw.length) return null;
  if (raw.startsWith("data:image/")) return raw;
  const mime = raw.startsWith("iVBOR")
    ? "image/png"
    : raw.startsWith("/9j/")
      ? "image/jpeg"
      : raw.startsWith("R0lGOD")
        ? "image/gif"
        : raw.startsWith("UklGR")
          ? "image/webp"
          : "image/png";
  return `data:${mime};base64,${raw}`;
}

function avatarNode(name: string, imageData: unknown, colorIndex?: number): HTMLElement {
  const wrap = makeEl("span", "hr-widget-avatar");
  const uri = dataUriForImage(imageData);
  if (uri) {
    const img = document.createElement("img");
    img.src = uri;
    img.alt = name;
    img.loading = "lazy";
    img.addEventListener("error", () => {
      const fallback = makeEl("span", `hr-widget-avatar-initials hr-tone-${colorIndex ?? toneOf(name)}`, initialsOf(name));
      wrap.replaceChildren(fallback);
    });
    wrap.append(img);
  } else {
    wrap.append(makeEl("span", `hr-widget-avatar-initials hr-tone-${colorIndex ?? toneOf(name)}`, initialsOf(name)));
  }
  return wrap;
}

/* ============================================================================
   1. Inline Organization Chart (`hr.org_chart`)
   ============================================================================ */
function renderOrgChart(env: WidgetEnvironment): WidgetHandle {
  const root = makeEl("section", "hr-widget hr-org-chart");
  const head = makeEl("div", "hr-widget-head");
  head.append(makeEl("h3", "hr-widget-title", "Organization Chart"));
  const body = makeEl("div", "hr-widget-body");
  root.append(head, body);

  let disposed = false;
  let seq = 0;

  const drawNode = (item: Obj, role: "manager" | "self" | "subordinate", depth: number): HTMLElement => {
    const id = asNum(item["id"]);
    const name = asStr(item["name"], `#${id}`);
    const jobTitle = asStr(item["job_title"]);
    const dept = asStr(item["department_name"]);
    const subCount = asNum(item["direct_sub_count"]);

    const row = makeEl("div", `hr-org-row is-${role}`);
    row.style.setProperty("--hr-depth", String(Math.min(depth, 5)));

    const card = makeEl("div", `hr-org-card is-${role}`);
    card.append(avatarNode(name, item["image_128"]));

    const info = makeEl("div", "hr-org-info");
    if (role !== "self" && env.openRecord && id > 0) {
      const link = makeButton(name, "hr-org-name-btn", () => {
        env.run(() => env.openRecord!("hr.employee", id));
      });
      info.append(link);
    } else {
      info.append(makeEl("div", "hr-org-name", name));
    }
    if (jobTitle) info.append(makeEl("div", "hr-org-sub", jobTitle));
    else if (dept) info.append(makeEl("div", "hr-org-sub", dept));
    card.append(info);

    if (subCount > 0) {
      card.append(makeEl("span", "hr-org-badge", String(subCount)));
    }
    row.append(card);
    return row;
  };

  const load = async (): Promise<void> => {
    const mine = ++seq;
    const id = recordIdOf(env);
    if (!id) {
      body.replaceChildren(makeEl("p", "hr-widget-empty", "Save this employee to display the organization chart."));
      return;
    }
    body.replaceChildren(makeEl("p", "hr-widget-loading", "Loading organization chart…"));
    try {
      const raw = await env.rpc.call("hr.employee", "get_org_chart", { ids: [id], context: env.context });
      if (disposed || mine !== seq) return;
      const data = asObj(raw);
      const managers = asList(data["managers"]);
      const self = asObj(data["self"]);
      const subordinates = asList(data["subordinates"]);

      const list = makeEl("div", "hr-org-list");
      managers.forEach((mgr, idx) => list.append(drawNode(mgr, "manager", idx)));
      list.append(drawNode(self, "self", managers.length));
      if (subordinates.length) {
        const subHeading = makeEl("div", "hr-org-section-label", `Direct Reports (${subordinates.length})`);
        list.append(subHeading);
        subordinates.forEach(sub => list.append(drawNode(sub, "subordinate", managers.length + 1)));
      }
      body.replaceChildren(...truncatedNotice(data, "The chart is cut at the server's limit: not every manager or direct report is shown."), list);
    } catch (error) {
      if (disposed || mine !== seq) return;
      body.replaceChildren(makeEl("p", "hr-widget-error", error instanceof Error ? error.message : String(error)));
    }
  };

  void load();
  const stop = env.onRecordEvent?.(event => {
    if (event.type === "saved" || event.type === "discarded") void load();
  });

  return {
    node: root,
    dispose() {
      disposed = true;
      stop?.();
    },
  };
}

/** The notice for a chart the server cut short; a result without `truncated: true` is complete. */
function truncatedNotice(data: Obj, text: string): HTMLElement[] {
  if (data["truncated"] !== true) return [];
  const notice = makeEl("p", "hr-widget-empty hr-chart-truncated", text);
  notice.setAttribute("role", "status");
  return [notice];
}

/* ============================================================================
   2. Inline Department Hierarchy Chart (`hr.department_chart`)
   ============================================================================ */
function renderDepartmentChart(env: WidgetEnvironment): WidgetHandle {
  const root = makeEl("section", "hr-widget hr-dept-chart");
  const head = makeEl("div", "hr-widget-head");
  head.append(makeEl("h3", "hr-widget-title", "Department Hierarchy"));
  const body = makeEl("div", "hr-widget-body");
  root.append(head, body);

  let disposed = false;
  let seq = 0;

  const drawDept = (item: Obj, role: "manager" | "self" | "subordinate", depth: number): HTMLElement => {
    const id = asNum(item["id"]);
    const name = asStr(item["name"], `#${id}`);
    const managerName = asStr(item["manager_name"]);
    const color = Math.max(0, Math.min(11, Math.trunc(asNum(item["color"], 0))));
    const empCount = asNum(item["total_employee"]);

    const row = makeEl("div", `hr-org-row is-${role}`);
    row.style.setProperty("--hr-depth", String(Math.min(depth, 5)));

    const card = makeEl("div", `hr-org-card is-${role} hr-dept-accent-${color}`);
    const info = makeEl("div", "hr-org-info");
    if (role !== "self" && env.openRecord && id > 0) {
      info.append(makeButton(name, "hr-org-name-btn", () => {
        env.run(() => env.openRecord!("hr.department", id));
      }));
    } else {
      info.append(makeEl("div", "hr-org-name", name));
    }
    if (managerName) info.append(makeEl("div", "hr-org-sub", `Manager: ${managerName}`));
    card.append(info);
    card.append(makeEl("span", "hr-org-badge", `${empCount}`));
    row.append(card);
    return row;
  };

  const load = async (): Promise<void> => {
    const mine = ++seq;
    const id = recordIdOf(env);
    if (!id) {
      body.replaceChildren(makeEl("p", "hr-widget-empty", "Save this department to display its hierarchy."));
      return;
    }
    body.replaceChildren(makeEl("p", "hr-widget-loading", "Loading department chart…"));
    try {
      const raw = await env.rpc.call("hr.department", "get_department_chart", { ids: [id], context: env.context });
      if (disposed || mine !== seq) return;
      const data = asObj(raw);
      const ancestors = asList(data["ancestors"]);
      const self = asObj(data["self"]);
      const children = asList(data["children"]);

      const list = makeEl("div", "hr-org-list");
      ancestors.forEach((anc, idx) => list.append(drawDept(anc, "manager", idx)));
      list.append(drawDept(self, "self", ancestors.length));
      if (children.length) {
        list.append(makeEl("div", "hr-org-section-label", `Sub-departments (${children.length})`));
        children.forEach(child => list.append(drawDept(child, "subordinate", ancestors.length + 1)));
      }
      body.replaceChildren(...truncatedNotice(data, "The chart is cut at the server's limit: not every parent or sub-department is shown."), list);
    } catch (error) {
      if (disposed || mine !== seq) return;
      body.replaceChildren(makeEl("p", "hr-widget-error", error instanceof Error ? error.message : String(error)));
    }
  };

  void load();
  const stop = env.onRecordEvent?.(event => {
    if (event.type === "saved" || event.type === "discarded") void load();
  });

  return {
    node: root,
    dispose() {
      disposed = true;
      stop?.();
    },
  };
}

/* ============================================================================
   3. Grouped Resume Timeline (`hr.resume_timeline`)
   ============================================================================ */
function renderResumeTimeline(env: WidgetEnvironment): WidgetHandle {
  const root = makeEl("section", "hr-widget hr-resume-timeline");
  const head = makeEl("div", "hr-widget-head");
  head.append(makeEl("h3", "hr-widget-title", "Resume"));
  const addBtn = makeButton("+ Add Entry", "erp-button hr-widget-add-btn", () => env.run(openAddModal));
  if (!env.field.readonly) head.append(addBtn);
  const body = makeEl("div", "hr-widget-body");
  root.append(head, body);

  let disposed = false;
  let seq = 0;
  let activeDialog: HTMLElement | undefined;

  const closeDialog = (): void => {
    activeDialog?.remove();
    activeDialog = undefined;
  };

  const openAddModal = async (): Promise<void> => {
    const employeeId = recordIdOf(env);
    if (!employeeId) return;
    closeDialog();
    const typesResp = await env.rpc.call("hr.resume.line.type", "web_search_read", {
      domain: [], specification: { name: {} }, limit: 50, order: "sequence asc, id asc", context: env.context,
    });
    if (disposed) return;
    const types = asList(asObj(typesResp)["records"]);

    const overlay = makeEl("div", "hr-dialog-overlay");
    const dialog = makeEl("div", "hr-dialog");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    const dlgHead = makeEl("div", "hr-dialog-header");
    dlgHead.append(makeEl("span", undefined, "Add Resume Entry"), makeButton("✕", "erp-button", closeDialog));
    const dlgBody = makeEl("div", "hr-dialog-body");

    const titleInput = document.createElement("input");
    titleInput.type = "text";
    titleInput.placeholder = "e.g. Senior Engineer at Arkana";
    const typeSelect = document.createElement("select");
    for (const item of types) {
      typeSelect.add(new Option(asStr(item["name"], `#${asNum(item["id"])}`), String(asNum(item["id"]))));
    }
    const startInput = document.createElement("input");
    startInput.type = "date";
    startInput.value = localDateIso();
    const endInput = document.createElement("input");
    endInput.type = "date";
    const descInput = document.createElement("textarea");
    descInput.rows = 3;
    descInput.placeholder = "Optional highlights or achievements";

    const fieldRow = (label: string, control: HTMLElement): HTMLElement => {
      const lbl = makeEl("label", "hr-dialog-field", label);
      lbl.append(control);
      return lbl;
    };
    dlgBody.append(
      fieldRow("Title", titleInput),
      fieldRow("Section", typeSelect),
      fieldRow("Start Date", startInput),
      fieldRow("End Date (optional)", endInput),
      fieldRow("Description", descInput),
    );
    const errText = makeEl("p", "hr-widget-error");
    errText.hidden = true;
    dlgBody.append(errText);

    const footer = makeEl("div", "hr-dialog-footer");
    footer.append(
      makeButton("Cancel", "erp-button", closeDialog),
      makeButton("Save Entry", "erp-button erp-button-primary", () => {
        const name = titleInput.value.trim();
        if (!name || !startInput.value) {
          errText.textContent = "Title and Start Date are required.";
          errText.hidden = false;
          return;
        }
        const vals: Record<string, unknown> = {
          employee_id: employeeId,
          name,
          date_start: startInput.value,
        };
        if (typeSelect.value) vals["line_type_id"] = Number(typeSelect.value);
        if (endInput.value) vals["date_end"] = endInput.value;
        if (descInput.value.trim()) vals["description"] = descInput.value.trim();
        env.run(async () => {
          await env.rpc.call("hr.resume.line", "create", { vals_list: [vals], context: env.context }, { write: true });
          closeDialog();
          await load();
        });
      }),
    );

    dialog.append(dlgHead, dlgBody, footer);
    overlay.append(dialog);
    overlay.addEventListener("click", e => { if (e.target === overlay) closeDialog(); });
    document.body.append(overlay);
    activeDialog = overlay;
    titleInput.focus();
  };

  const load = async (): Promise<void> => {
    const mine = ++seq;
    const employeeId = recordIdOf(env);
    addBtn.disabled = !employeeId;
    if (!employeeId) {
      body.replaceChildren(makeEl("p", "hr-widget-empty", "Save this employee to add resume entries."));
      return;
    }
    try {
      const resp = await env.rpc.call("hr.resume.line", "web_search_read", {
        domain: [["employee_id", "=", employeeId]],
        specification: {
          name: {},
          line_type_id: { fields: { display_name: {} } },
          date_start: {},
          date_end: {},
          description: {},
          is_course: {},
          duration_hours: {},
        },
        order: "date_start desc, id desc",
        limit: 100,
        context: env.context,
      });
      if (disposed || mine !== seq) return;
      const rows = asList(asObj(resp)["records"]);
      if (!rows.length) {
        body.replaceChildren(makeEl("p", "hr-widget-empty", "No resume entries yet."));
        return;
      }
      const groups = new Map<string, Obj[]>();
      for (const row of rows) {
        const key = m2oLabel(row["line_type_id"], "Experience");
        const bucket = groups.get(key) ?? [];
        bucket.push(row);
        groups.set(key, bucket);
      }

      const container = makeEl("div", "hr-timeline-groups");
      for (const [sectionName, items] of groups.entries()) {
        const group = makeEl("div", "hr-timeline-group");
        group.append(makeEl("h4", "hr-timeline-section", sectionName));
        const rail = makeEl("div", "hr-timeline-rail");
        for (const item of items) {
          const itemId = asNum(item["id"]);
          const start = asStr(item["date_start"]);
          const end = asStr(item["date_end"]) || "Present";
          const desc = asStr(item["description"]);
          const hours = asNum(item["duration_hours"]);

          const entry = makeEl("article", "hr-timeline-item");
          entry.append(makeEl("span", "hr-timeline-dot"));
          const content = makeEl("div", "hr-timeline-content");
          const top = makeEl("div", "hr-timeline-top");
          const titleText = asStr(item["name"], `#${itemId}`);
          if (env.openRecord && itemId > 0) {
            top.append(makeButton(titleText, "hr-timeline-title-btn", () => {
              env.run(() => env.openRecord!("hr.resume.line", itemId));
            }));
          } else {
            top.append(makeEl("strong", "hr-timeline-title", titleText));
          }
          top.append(makeEl("span", "hr-timeline-dates", `${start} – ${end}`));
          if (!env.field.readonly && itemId > 0) {
            const del = makeButton("×", "hr-timeline-del", () => {
              env.run(async () => {
                if (!await confirmRemoval(env, "Remove resume entry", `Remove "${titleText}" from the resume?`)) return;
                await env.rpc.call("hr.resume.line", "unlink", { ids: [itemId], context: env.context }, { write: true });
                await load();
              });
            });
            del.title = "Remove entry";
            del.setAttribute("aria-label", `Remove ${titleText}`);
            top.append(del);
          }
          content.append(top);
          if (hours > 0) content.append(makeEl("div", "hr-timeline-meta", `${hours} hours`));
          if (desc) content.append(makeEl("p", "hr-timeline-desc", desc));
          entry.append(content);
          rail.append(entry);
        }
        group.append(rail);
        container.append(group);
      }
      body.replaceChildren(container);
    } catch (error) {
      if (disposed || mine !== seq) return;
      body.replaceChildren(makeEl("p", "hr-widget-error", error instanceof Error ? error.message : String(error)));
    }
  };

  void load();
  const stop = env.onRecordEvent?.(event => {
    if (event.type === "saved" || event.type === "discarded") void load();
  });

  return {
    node: root,
    dispose() {
      disposed = true;
      closeDialog();
      stop?.();
    },
  };
}

/* ============================================================================
   4. Grouped Skills & Certifications Panel (`hr.skills_panel`)
   ============================================================================ */
function renderSkillsPanel(env: WidgetEnvironment): WidgetHandle {
  const root = makeEl("section", "hr-widget hr-skills-panel");
  const head = makeEl("div", "hr-widget-head");
  head.append(makeEl("h3", "hr-widget-title", "Skills"));
  const addBtn = makeButton("+ Add Skill", "erp-button hr-widget-add-btn", () => env.run(openAddModal));
  if (!env.field.readonly) head.append(addBtn);
  const body = makeEl("div", "hr-widget-body");
  root.append(head, body);

  let disposed = false;
  let seq = 0;
  let activeDialog: HTMLElement | undefined;

  const closeDialog = (): void => {
    activeDialog?.remove();
    activeDialog = undefined;
  };

  const openAddModal = async (): Promise<void> => {
    const employeeId = recordIdOf(env);
    if (!employeeId) return;
    closeDialog();
    const [typesResp, skillsResp, levelsResp] = await Promise.all([
      env.rpc.call("hr.skill.type", "web_search_read", {
        domain: [], specification: { name: {}, is_certification: {} }, limit: 50, order: "name asc", context: env.context,
      }),
      env.rpc.call("hr.skill", "web_search_read", {
        domain: [], specification: { name: {}, skill_type_id: { fields: { display_name: {} } } }, limit: 200, order: "sequence asc, name asc", context: env.context,
      }),
      env.rpc.call("hr.skill.level", "web_search_read", {
        domain: [], specification: { name: {}, level_progress: {}, skill_type_id: { fields: { display_name: {} } } }, limit: 200, order: "level_progress desc, name asc", context: env.context,
      }),
    ]);
    if (disposed) return;

    const types = asList(asObj(typesResp)["records"]);
    const allSkills = asList(asObj(skillsResp)["records"]);
    const allLevels = asList(asObj(levelsResp)["records"]);

    const typeIdOf = (row: Obj): number => {
      const raw = row["skill_type_id"];
      if (typeof raw === "number") return raw;
      if (raw && typeof raw === "object" && !Array.isArray(raw)) return asNum((raw as Obj)["id"]);
      return 0;
    };

    const overlay = makeEl("div", "hr-dialog-overlay");
    const dialog = makeEl("div", "hr-dialog");
    dialog.setAttribute("role", "dialog");
    dialog.setAttribute("aria-modal", "true");
    const dlgHead = makeEl("div", "hr-dialog-header");
    dlgHead.append(makeEl("span", undefined, "Add Skill or Certification"), makeButton("✕", "erp-button", closeDialog));
    const dlgBody = makeEl("div", "hr-dialog-body");

    const typeSelect = document.createElement("select");
    for (const item of types) typeSelect.add(new Option(asStr(item["name"]), String(asNum(item["id"]))));
    const skillSelect = document.createElement("select");
    const levelSelect = document.createElement("select");
    const validFrom = document.createElement("input");
    validFrom.type = "date";
    const validTo = document.createElement("input");
    validTo.type = "date";

    const syncOptions = (): void => {
      const tid = Number(typeSelect.value);
      skillSelect.replaceChildren();
      for (const s of allSkills.filter(r => typeIdOf(r) === tid)) {
        skillSelect.add(new Option(asStr(s["name"]), String(asNum(s["id"]))));
      }
      levelSelect.replaceChildren();
      for (const l of allLevels.filter(r => typeIdOf(r) === tid)) {
        levelSelect.add(new Option(`${asStr(l["name"])} (${asNum(l["level_progress"])}%)`, String(asNum(l["id"]))));
      }
    };
    typeSelect.addEventListener("change", syncOptions);
    syncOptions();

    const fieldRow = (label: string, control: HTMLElement): HTMLElement => {
      const lbl = makeEl("label", "hr-dialog-field", label);
      lbl.append(control);
      return lbl;
    };
    dlgBody.append(
      fieldRow("Skill Type", typeSelect),
      fieldRow("Skill", skillSelect),
      fieldRow("Level", levelSelect),
      fieldRow("Valid From (optional)", validFrom),
      fieldRow("Valid Until (optional)", validTo),
    );
    const errText = makeEl("p", "hr-widget-error");
    errText.hidden = true;
    dlgBody.append(errText);

    const footer = makeEl("div", "hr-dialog-footer");
    footer.append(
      makeButton("Cancel", "erp-button", closeDialog),
      makeButton("Save Skill", "erp-button erp-button-primary", () => {
        if (!typeSelect.value || !skillSelect.value || !levelSelect.value) {
          errText.textContent = "Please choose a skill type, skill, and level.";
          errText.hidden = false;
          return;
        }
        const vals: Record<string, unknown> = {
          employee_id: employeeId,
          skill_type_id: Number(typeSelect.value),
          skill_id: Number(skillSelect.value),
          skill_level_id: Number(levelSelect.value),
        };
        if (validFrom.value) vals["valid_from"] = validFrom.value;
        if (validTo.value) vals["valid_to"] = validTo.value;
        env.run(async () => {
          await env.rpc.call("hr.employee.skill", "create", { vals_list: [vals], context: env.context }, { write: true });
          closeDialog();
          await load();
        });
      }),
    );

    dialog.append(dlgHead, dlgBody, footer);
    overlay.append(dialog);
    overlay.addEventListener("click", e => { if (e.target === overlay) closeDialog(); });
    document.body.append(overlay);
    activeDialog = overlay;
    typeSelect.focus();
  };

  const load = async (): Promise<void> => {
    const mine = ++seq;
    const employeeId = recordIdOf(env);
    addBtn.disabled = !employeeId;
    if (!employeeId) {
      body.replaceChildren(makeEl("p", "hr-widget-empty", "Save this employee to manage skills."));
      return;
    }
    try {
      const resp = await env.rpc.call("hr.employee.skill", "web_search_read", {
        domain: [["employee_id", "=", employeeId]],
        specification: {
          skill_type_id: { fields: { display_name: {} } },
          skill_id: { fields: { display_name: {} } },
          skill_level_id: { fields: { display_name: {} } },
          level_progress: {},
          is_certification: {},
          valid_from: {},
          valid_to: {},
        },
        order: "skill_type_id asc, level_progress desc, id asc",
        limit: 100,
        context: env.context,
      });
      if (disposed || mine !== seq) return;
      const rows = asList(asObj(resp)["records"]);
      if (!rows.length) {
        body.replaceChildren(makeEl("p", "hr-widget-empty", "No skills recorded yet."));
        return;
      }
      const groups = new Map<string, Obj[]>();
      for (const row of rows) {
        const key = m2oLabel(row["skill_type_id"], "Skills");
        const bucket = groups.get(key) ?? [];
        bucket.push(row);
        groups.set(key, bucket);
      }

      const container = makeEl("div", "hr-skills-groups");
      for (const [typeName, items] of groups.entries()) {
        const group = makeEl("div", "hr-skills-group");
        group.append(makeEl("h4", "hr-timeline-section", typeName));
        const list = makeEl("div", "hr-skills-list");
        for (const item of items) {
          const itemId = asNum(item["id"]);
          const skillName = m2oLabel(item["skill_id"], `#${itemId}`);
          const levelName = m2oLabel(item["skill_level_id"]);
          const progress = Math.max(0, Math.min(100, Math.round(asNum(item["level_progress"]))));
          const validFrom = asStr(item["valid_from"]);
          const validTo = asStr(item["valid_to"]);

          const rowEl = makeEl("div", "hr-skill-row");
          const top = makeEl("div", "hr-skill-row-top");
          top.append(makeEl("span", "hr-skill-name", skillName));
          const right = makeEl("div", "hr-skill-right");
          if (levelName) right.append(makeEl("span", "hr-skill-level", levelName));
          right.append(makeEl("span", "hr-skill-pct", `${progress}%`));
          if (!env.field.readonly && itemId > 0) {
            const del = makeButton("×", "hr-timeline-del", () => {
              env.run(async () => {
                if (!await confirmRemoval(env, "Remove skill", `Remove the skill "${skillName}"?`)) return;
                await env.rpc.call("hr.employee.skill", "unlink", { ids: [itemId], context: env.context }, { write: true });
                await load();
              });
            });
            del.title = "Remove skill";
            del.setAttribute("aria-label", `Remove ${skillName}`);
            right.append(del);
          }
          top.append(right);

          const track = makeEl("div", "field-progressbar-track");
          const fill = makeEl("div", "field-progressbar-fill");
          fill.style.width = `${progress}%`;
          track.append(fill);

          rowEl.append(top, track);
          if (validFrom || validTo) {
            rowEl.append(makeEl("div", "hr-skill-validity", `Valid: ${validFrom || "…"} → ${validTo || "Ongoing"}`));
          }
          list.append(rowEl);
        }
        group.append(list);
        container.append(group);
      }
      body.replaceChildren(container);
    } catch (error) {
      if (disposed || mine !== seq) return;
      body.replaceChildren(makeEl("p", "hr-widget-error", error instanceof Error ? error.message : String(error)));
    }
  };

  void load();
  const stop = env.onRecordEvent?.(event => {
    if (event.type === "saved" || event.type === "discarded") void load();
  });

  return {
    node: root,
    dispose() {
      disposed = true;
      closeDialog();
      stop?.();
    },
  };
}

registerFieldWidget("hr.org_chart", { kinds: ["one2many"], render: renderOrgChart });
registerFieldWidget("hr.department_chart", { kinds: ["one2many"], render: renderDepartmentChart });
registerFieldWidget("hr.resume_timeline", { kinds: ["one2many"], render: renderResumeTimeline });
registerFieldWidget("hr.skills_panel", { kinds: ["one2many"], render: renderSkillsPanel });
