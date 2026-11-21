/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The filter bar of the report screen (design REPORTING.CORE §12): date, comparison, companies, options, search.
// Each control edits a copy and hands a patch of the options to `apply`; the screen sends the whole options back.
import { createDateTimeEditor, createDateTimeRangeEditor, icon } from "@erpforall/client";
import type { ClientActionEnvironment, IconName } from "@erpforall/client";
import { formatDate, obj, type Filters, type Options } from "./report-data.js";

export interface FilterContext {
  readonly env: ClientActionEnvironment; readonly options: Options; readonly filters: Filters;
  /** Merge `patch` into the options and reload the report. */
  apply(patch: Options): void;
}
export interface Disclosure { open(): void; close(restoreFocus?: boolean): void; isOpen(): boolean; dispose(): void }

export const message = (error: unknown): string => error instanceof Error ? error.message : String(error);

/** Button plus panel: Escape and a click or focus outside close it; the trigger says whether it is open. */
export function disclosure(trigger: HTMLElement, panel: HTMLElement, role: "group" | "menu", toggleOnClick = true): Disclosure {
  let isOpen = false;
  panel.hidden = true; panel.setAttribute("role", role);
  trigger.setAttribute("aria-haspopup", role === "menu" ? "menu" : "true"); trigger.setAttribute("aria-expanded", "false");
  const close = (restoreFocus = true): void => {
    if (!isOpen) return;
    isOpen = false; panel.hidden = true; trigger.setAttribute("aria-expanded", "false");
    if (restoreFocus) trigger.focus();
  };
  const open = (): void => { if (isOpen) return; isOpen = true; panel.hidden = false; trigger.setAttribute("aria-expanded", "true"); };
  const outside = (event: Event): void => {
    const target = event.target;
    if (isOpen && target instanceof Node && !panel.contains(target) && !trigger.contains(target)) close(false);
  };
  const escape = (event: KeyboardEvent): void => { if (isOpen && event.key === "Escape") { event.stopPropagation(); close(true); } };
  const leave = (event: FocusEvent): void => {
    const next = event.relatedTarget;
    if (isOpen && next instanceof Node && !panel.contains(next) && !trigger.contains(next)) close(false);
  };
  const toggle = (): void => { if (isOpen) close(true); else open(); };
  const doc = trigger.ownerDocument;
  doc.addEventListener("pointerdown", outside);
  if (toggleOnClick) trigger.addEventListener("click", toggle);
  trigger.addEventListener("keydown", escape);
  panel.addEventListener("keydown", escape); panel.addEventListener("focusout", leave);
  return { open, close, isOpen: () => isOpen, dispose: () => {
    doc.removeEventListener("pointerdown", outside); if (toggleOnClick) trigger.removeEventListener("click", toggle); trigger.removeEventListener("keydown", escape);
    panel.removeEventListener("keydown", escape); panel.removeEventListener("focusout", leave);
  } };
}

const PRESETS: readonly (readonly [string, string])[] = [
  ["today", "Today"], ["this_month", "This month"], ["last_month", "Last month"], ["this_quarter", "This quarter"],
  ["last_quarter", "Last quarter"], ["this_year", "This year"], ["last_year", "Last year"], ["custom", "Custom"]
];
const COMPARISONS: readonly (readonly [string, string])[] = [
  ["none", "No comparison"], ["previous_period", "Previous period"], ["same_last_year", "Same period last year"], ["custom", "Custom period"]
];
const label = (table: readonly (readonly [string, string])[], key: string): string => table.find(([k]) => k === key)?.[1] ?? key;

function field(text: string, control: HTMLElement): HTMLElement {
  const wrapper = document.createElement("label"); wrapper.className = "report-field";
  const caption = document.createElement("span"); caption.textContent = text;
  wrapper.append(caption, control); return wrapper;
}
function select(name: string, choices: readonly (readonly [string, string])[], value: string): HTMLSelectElement {
  const node = document.createElement("select"); node.name = name;
  for (const [key, text] of choices) { const option = document.createElement("option"); option.value = key; option.textContent = text; node.append(option); }
  node.value = value; return node;
}
function checkbox(name: string, text: string, checked: boolean): HTMLLabelElement {
  const wrapper = document.createElement("label"); wrapper.className = "report-check";
  const input = document.createElement("input"); input.type = "checkbox"; input.name = name; input.checked = checked;
  wrapper.append(input, document.createTextNode(text)); return wrapper;
}
function actionButton(text: string, run: () => void, kind = ""): HTMLButtonElement {
  const node = document.createElement("button"); node.type = "button"; node.className = `erp-button report-button ${kind}`.trim();
  node.textContent = text; node.addEventListener("click", run); return node;
}
const textOf = (value: unknown): string | null => typeof value === "string" ? value : null;

interface Chip { readonly node: HTMLElement; dispose(): void }
/** A trigger and its panel; `build` fills the panel and may return what to dispose with the bar. */
function chip(name: string, caption: string, iconName: IconName | null, build: (panel: HTMLElement, close: () => void) => (() => void)[]): Chip {
  const node = document.createElement("div"); node.className = "report-filter"; node.dataset["filter"] = name;
  const trigger = actionButton(caption, () => {}, "report-filter-button");
  if (iconName) trigger.prepend(icon(iconName));
  const panel = document.createElement("div"); panel.className = "report-panel"; panel.setAttribute("aria-label", caption);
  const toggle = disclosure(trigger, panel, "group");
  const cleanups = build(panel, () => toggle.close(true));
  node.append(trigger, panel);
  return { node, dispose: () => { toggle.dispose(); cleanups.forEach(run => run()); } };
}

function periodText(options: Options, lang: string): string {
  const date = obj(options["date"], "the date option"), to = textOf(date["to"]), from = textOf(date["from"]);
  if (!to) return "";
  return from ? `${formatDate(from, lang)} – ${formatDate(to, lang)}` : formatDate(to, lang);
}

function dateChip(ctx: FilterContext): Chip {
  const { env, options, filters } = ctx, lang = env.editorLocale.lang;
  const date = obj(options["date"], "the date option"), mode = filters.dateMode, preset = textOf(date["preset"]) ?? "custom";
  const from = textOf(date["from"]), to = textOf(date["to"]);
  const period = periodText(options, lang);
  return chip("date", preset === "custom" && period ? period : label(PRESETS, preset), "calendar", (panel, close) => {
    const presets = PRESETS.filter(([key]) => key !== "today" || mode === "single");
    const picker = select("preset", presets, preset); const custom = document.createElement("div"); custom.className = "report-custom";
    custom.hidden = preset !== "custom";
    panel.append(field("Period", picker), custom);
    picker.addEventListener("change", () => {
      if (picker.value === "custom") { custom.hidden = false; return; }
      close(); ctx.apply({ date: { mode, preset: picker.value, from: null, to: null } });
    });
    const common = { kind: "date" as const, locale: env.editorLocale, popovers: env.messages.popovers, report: (error: unknown) => env.notice(message(error)) };
    const editor = mode === "range"
      ? createDateTimeRangeEditor({ ...common, names: ["date_from", "date_to"], labels: ["From", "To"], value: [from ?? false, to ?? false], required: [true, true],
        onApply: ([start, end]) => { if (start === false || end === false) return; close(); ctx.apply({ date: { mode, preset: "custom", from: start, to: end } }); } })
      : createDateTimeEditor({ ...common, name: "date_to", label: "Date", value: to ?? false, required: true,
        onApply: end => { if (end === false) return; close(); ctx.apply({ date: { mode, preset: "custom", from: null, to: end } }); } });
    custom.append(editor.node);
    return [() => editor.dispose()];
  });
}

function comparisonChip(ctx: FilterContext): Chip {
  const { env, options } = ctx;
  const given = obj(options["comparison"], "the comparison option");
  const mode = textOf(given["mode"]) ?? "none", periods = typeof given["periods"] === "number" ? given["periods"] : 1, order = textOf(given["order"]) ?? "descending";
  const caption = mode === "none" ? "Compare" : `${label(COMPARISONS, mode)}${mode === "custom" ? "" : ` (${periods})`}`;
  return chip("comparison", caption, null, (panel, close) => {
    const picker = select("comparison_mode", COMPARISONS, mode);
    const count = document.createElement("input"); count.type = "number"; count.name = "comparison_periods"; count.min = "1"; count.step = "1"; count.value = String(periods);
    const sort = select("comparison_order", [["descending", "Newest first"], ["ascending", "Oldest first"]], order);
    const range = document.createElement("div"); range.className = "report-custom";
    const counted = field("Periods", count), sorted = field("Order", sort);
    let draft: [string | false, string | false] = [textOf(given["from"]) ?? false, textOf(given["to"]) ?? false];
    const common = { kind: "date" as const, locale: env.editorLocale, popovers: env.messages.popovers, report: (error: unknown) => env.notice(message(error)) };
    const editor = createDateTimeRangeEditor({ ...common, names: ["comparison_from", "comparison_to"], labels: ["From", "To"], value: draft, onApply: value => { draft = [value[0], value[1]]; } });
    range.append(editor.node);
    const show = (): void => { counted.hidden = sorted.hidden = picker.value === "none" || picker.value === "custom"; range.hidden = picker.value !== "custom"; };
    show(); picker.addEventListener("change", show);
    const submit = actionButton("Apply", () => {
      const patch: Options = { mode: picker.value, periods: Number(count.value), order: sort.value };
      if (picker.value === "custom") {
        if (draft[0] === false || draft[1] === false) { env.notice("Enter both dates of the comparison period."); return; }
        patch["from"] = draft[0]; patch["to"] = draft[1];
      }
      close(); ctx.apply({ comparison: patch });
    }, "erp-button-primary");
    panel.append(field("Compare with", picker), counted, sorted, range, submit);
    return [() => editor.dispose()];
  });
}

function companiesChip(ctx: FilterContext): Chip {
  const { env, options } = ctx;
  const chosen = new Set(Array.isArray(options["companies"]) ? options["companies"] as unknown[] : []);
  return chip("companies", `Companies (${chosen.size})`, "building", (panel, close) => {
    const boxes = env.companies.map(company => ({ company, row: checkbox("company", company.name, chosen.has(company.id)) }));
    const submit = actionButton("Apply", () => {
      const ids = boxes.filter(({ row }) => row.querySelector("input")?.checked).map(({ company }) => company.id);
      if (!ids.length) { env.notice("Select at least one company."); return; }
      close(); ctx.apply({ companies: ids });
    }, "erp-button-primary");
    panel.append(...boxes.map(({ row }) => row), submit);
    return [];
  });
}

function optionsChip(ctx: FilterContext, provider: Options): Chip {
  const { options, filters } = ctx;
  return chip("options", "Options", "sliders-horizontal", (panel, close) => {
    if (filters.partnerLedger) {
      const row = checkbox("unreconciled", "Unreconciled at period end", provider["unreconciled"] === true);
      const input = row.querySelector("input")!;
      input.addEventListener("change", () => {
        close();
        ctx.apply({ provider: { ...provider, unreconciled: input.checked } });
      });
      panel.append(row);
    }
    if (filters.generalLedger && typeof provider["fiscal_opening"] === "boolean") {
      const row = checkbox("fiscal_opening", "Use fiscal-year opening", provider["fiscal_opening"]);
      const input = row.querySelector("input")!;
      input.addEventListener("change", () => {
        close();
        ctx.apply({ provider: { ...provider, fiscal_opening: input.checked }, unfolded: [], loaded_pages: {} });
      });
      panel.append(row);
    }
    if (filters.unfoldAll) {
      const row = checkbox("unfold_all", "Unfold all lines", options["unfold_all"] === true);
      row.querySelector("input")?.addEventListener("change", event => { close(); ctx.apply({ unfold_all: (event.currentTarget as HTMLInputElement).checked }); });
      panel.append(row);
    }
    if (filters.hideZero !== "never") {
      const row = checkbox("hide_zero", "Hide lines at zero", options["hide_zero"] === true);
      row.querySelector("input")?.addEventListener("change", event => { close(); ctx.apply({ hide_zero: (event.currentTarget as HTMLInputElement).checked }); });
      panel.append(row);
    }
    return [];
  });
}

function searchForm(ctx: FilterContext): HTMLFormElement {
  const form = document.createElement("form"); form.className = "report-search"; form.setAttribute("role", "search");
  const input = document.createElement("input"); input.type = "search"; input.name = "search"; input.placeholder = "Search lines"; input.setAttribute("aria-label", "Search lines");
  input.value = typeof ctx.options["search"] === "string" ? ctx.options["search"] : "";
  form.append(input);
  form.addEventListener("submit", event => { event.preventDefault(); ctx.apply({ search: input.value.trim() }); });
  return form;
}

/** The bar for `ctx`; dispose it before building the next one (the editors inside own listeners). */
export function buildFilters(ctx: FilterContext): { node: HTMLElement; dispose(): void } {
  const node = document.createElement("div"); node.className = "report-filters"; node.setAttribute("role", "toolbar"); node.setAttribute("aria-label", "Report filters");
  const chips: Chip[] = ctx.filters.currentOnly ? [] : [dateChip(ctx)];
  if (ctx.filters.comparison) chips.push(comparisonChip(ctx));
  if (ctx.filters.multiCompany && ctx.env.companies.length > 1) chips.push(companiesChip(ctx));
  const provider = ctx.filters.partnerLedger || ctx.filters.generalLedger ? obj(ctx.options["provider"], "the report provider options") : {};
  if (ctx.filters.unfoldAll || ctx.filters.hideZero !== "never" || ctx.filters.partnerLedger || (ctx.filters.generalLedger && "fiscal_opening" in provider)) {
    chips.push(optionsChip(ctx, provider));
  }
  node.append(...chips.map(c => c.node));
  if (ctx.filters.searchBar) node.append(searchForm(ctx));
  return { node, dispose: () => chips.forEach(c => c.dispose()) };
}
