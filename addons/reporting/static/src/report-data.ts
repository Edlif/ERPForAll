/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The wire of `reporting.report` (design REPORTING.CORE §6, §9): decoders that refuse a malformed
// answer by name, and the pure operations on the loaded line list (fold, unfold, load more).
export type CellKind = "monetary" | "decimal" | "integer" | "percent" | "date" | "text" | "empty";
export type LineKind = "section" | "group" | "detail" | "total" | "more";
export interface Cell { readonly kind: CellKind; readonly value: string | null; readonly scale: number; readonly zero: boolean }
export interface Line {
  readonly id: string; readonly parent: string | null; readonly name: string; readonly level: number; readonly kind: LineKind;
  readonly unfoldable: boolean; unfolded: boolean; readonly cells: readonly Cell[]; readonly caret: readonly string[];
  readonly remaining: number | null; readonly nextOffset: number | null;
}
export interface Column { readonly group: number; readonly name: string; readonly label: string; readonly kind: CellKind; readonly sortable: boolean }
export interface Period { readonly from: string | null; readonly to: string }
export interface Filters {
  readonly dateMode: "range" | "single"; readonly comparison: boolean; readonly multiCompany: boolean; readonly unfoldAll: boolean;
  readonly hideZero: "never" | "optional" | "by_default"; readonly searchBar: boolean; readonly totalsBelow: boolean;
}
/** The options object as the server resolved it; the screen edits it and sends it back whole (design §5). */
export type Options = Record<string, unknown>;
export interface Loaded { readonly options: Options; readonly lines: Line[]; readonly computedAt: string; readonly hidden: readonly string[] }

const KINDS: readonly string[] = ["monetary", "decimal", "integer", "percent", "date", "text", "empty"];
const LINE_KINDS: readonly string[] = ["section", "group", "detail", "total", "more"];

export function obj(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) throw new Error(`The server answered ${what} in an unknown form.`);
  return value as Record<string, unknown>;
}
function arr(value: unknown, what: string): unknown[] {
  if (!Array.isArray(value)) throw new Error(`The server answered ${what} in an unknown form.`);
  return value;
}
function str(value: unknown, what: string): string {
  if (typeof value !== "string") throw new Error(`The server answered ${what} in an unknown form.`);
  return value;
}
function num(value: unknown, what: string): number {
  if (typeof value !== "number" || !Number.isInteger(value)) throw new Error(`The server answered ${what} in an unknown form.`);
  return value;
}
function bool(value: unknown, what: string): boolean {
  if (typeof value !== "boolean") throw new Error(`The server answered ${what} in an unknown form.`);
  return value;
}
const orNull = <T>(value: unknown, read: (v: unknown) => T): T | null => value === null || value === undefined ? null : read(value);

function cell(raw: unknown): Cell {
  const c = obj(raw, "a cell"), kind = str(c["kind"], "a cell kind");
  if (!KINDS.includes(kind)) throw new Error(`The server sent a cell of unknown kind '${kind}'.`);
  const value = orNull(c["value"], v => str(v, "a cell value"));
  return { kind: kind as CellKind, value, scale: num(c["scale"], "a cell scale"), zero: bool(c["zero"], "a cell") };
}
export function line(raw: unknown): Line {
  const l = obj(raw, "a line"), kind = str(l["kind"], "a line kind");
  if (!LINE_KINDS.includes(kind)) throw new Error(`The server sent a line of unknown kind '${kind}'.`);
  return {
    id: str(l["id"], "a line id"), parent: orNull(l["parent"], v => str(v, "a line parent")), name: str(l["name"], "a line name"),
    level: num(l["level"], "a line level"), kind: kind as LineKind, unfoldable: bool(l["unfoldable"], "a line"), unfolded: bool(l["unfolded"], "a line"),
    cells: arr(l["cells"], "line cells").map(cell), caret: arr(l["caret"], "line caret items").map(item => str(item, "a caret item")),
    remaining: orNull(l["remaining"], v => num(v, "a remaining count")), nextOffset: orNull(l["next_offset"], v => num(v, "a next offset"))
  };
}
export function filters(raw: unknown): Filters {
  const f = obj(raw, "the filters"), mode = str(f["date_mode"], "the date mode"), hide = str(f["hide_zero"], "the hide-zero mode");
  if (mode !== "range" && mode !== "single" || hide !== "never" && hide !== "optional" && hide !== "by_default") throw new Error("The server sent unknown filter modes.");
  return { dateMode: mode, hideZero: hide, comparison: bool(f["comparison"], "the filters"), multiCompany: bool(f["multi_company"], "the filters"),
    unfoldAll: bool(f["unfold_all"], "the filters"), searchBar: bool(f["search_bar"], "the filters"), totalsBelow: bool(f["totals_below"], "the filters") };
}
export function decodeOptions(raw: unknown): { name: string; options: Options; filters: Filters } {
  const a = obj(raw, "the report options");
  return { name: str(a["name"], "the report name"), options: obj(a["options"], "the options"), filters: filters(a["filters"]) };
}
export function decodeLines(raw: unknown): Loaded {
  const a = obj(raw, "the report lines");
  return { options: obj(a["options"], "the options"), lines: arr(a["lines"], "the lines").map(line), computedAt: str(a["computed_at"], "the computation time"),
    hidden: arr(a["hidden_lines"], "the hidden lines").map(name => str(name, "a hidden line name")) };
}
export function decodeExpand(raw: unknown): Line[] { return arr(obj(raw, "an unfold")["lines"], "the lines").map(line); }

export function columns(options: Options): Column[] {
  return arr(options["columns"], "the columns").map(raw => {
    const c = obj(raw, "a column"), kind = str(c["kind"], "a column kind");
    if (!KINDS.includes(kind)) throw new Error(`The server sent a column of unknown kind '${kind}'.`);
    return { group: num(c["group"], "a column group"), name: str(c["name"], "a column name"), label: str(c["label"], "a column label"), kind: kind as CellKind, sortable: bool(c["sortable"], "a column") };
  });
}
export function periods(options: Options): Period[] {
  return arr(options["column_groups"], "the column groups").map(raw => {
    const p = obj(raw, "a column group");
    return { from: orNull(p["from"], v => str(v, "a period start")), to: str(p["to"], "a period end") };
  });
}
export const unfolded = (options: Options): string[] => arr(options["unfolded"], "the unfolded lines").map(id => str(id, "an unfolded line id"));

/** The lines below `id`: ids are the parent's id plus a segment, so a prefix with the separator is exact. */
const below = (id: string): string => `${id}|`;

/** `id` folded: its descendants (and the totals and load-more rows among them) leave the list and the unfolded set. */
export function fold(lines: Line[], opened: readonly string[], id: string): { lines: Line[]; unfolded: string[] } {
  const kept = lines.filter(l => !l.id.startsWith(below(id)));
  const self = kept.find(l => l.id === id);
  if (!self) throw new Error("This line is no longer in the report. Refresh the report.");
  self.unfolded = false;
  return { lines: kept, unfolded: opened.filter(open => open !== id && !open.startsWith(below(id))) };
}
/** `children` follow `id` right away (an unfold). */
export function unfold(lines: Line[], opened: readonly string[], id: string, children: readonly Line[]): { lines: Line[]; unfolded: string[] } {
  const at = lines.findIndex(l => l.id === id);
  const self = lines[at];
  if (!self) throw new Error("This line is no longer in the report. Refresh the report.");
  self.unfolded = true;
  const next = [...lines]; next.splice(at + 1, 0, ...children);
  return { lines: next, unfolded: opened.includes(id) ? [...opened] : [...opened, id] };
}
/** The load-more row `id` replaced by the next page. */
export function loadMore(lines: Line[], id: string, page: readonly Line[]): Line[] {
  const at = lines.findIndex(l => l.id === id);
  if (at < 0 || lines[at]?.kind !== "more") throw new Error("This page is no longer in the report. Refresh the report.");
  const next = [...lines]; next.splice(at, 1, ...page);
  return next;
}

/** The next sort of `label` after a click: none, descending, ascending, none (design §12). */
export function nextOrder(options: Options, label: string): { column: string; direction: "desc" | "asc" } | null {
  const order = options["order"];
  const current = order && typeof order === "object" ? order as Record<string, unknown> : null;
  if (!current || current["column"] !== label) return { column: label, direction: "desc" };
  return current["direction"] === "desc" ? { column: label, direction: "asc" } : null;
}

const separators = (lang: string): { group: string; decimal: string } => {
  let parts: Intl.NumberFormatPart[];
  try { parts = new Intl.NumberFormat(lang.replace("_", "-")).formatToParts(1234567.5); } catch { parts = new Intl.NumberFormat("en").formatToParts(1234567.5); }
  return { group: parts.find(p => p.type === "group")?.value ?? ",", decimal: parts.find(p => p.type === "decimal")?.value ?? "." };
};
/** A decimal string shown in the user's locale without passing through a binary float (design §6: exact numbers). */
export function formatCell(c: Cell, lang: string): string {
  if (c.value === null || c.kind === "empty") return "";
  if (c.kind !== "monetary" && c.kind !== "decimal" && c.kind !== "integer" && c.kind !== "percent") return c.value;
  const match = /^(-?)(\d+)(?:\.(\d+))?$/.exec(c.value);
  if (!match) return c.value;
  const { group, decimal } = separators(lang);
  const whole = match[2]!.replace(/\B(?=(\d{3})+(?!\d))/g, group);
  return `${match[1]}${whole}${match[3] ? decimal + match[3] : ""}${c.kind === "percent" ? " %" : ""}`;
}
export function formatDate(iso: string, lang: string): string {
  const date = new Date(`${iso}T00:00:00Z`);
  if (Number.isNaN(date.getTime())) return iso;
  try { return new Intl.DateTimeFormat(lang.replace("_", "-"), { dateStyle: "medium", timeZone: "UTC" }).format(date); } catch { return iso; }
}
