/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The dynamic report screen (design REPORTING.CORE §12): options and filter bar, the line tree with unfold,
// load more, sort, caret menu and drill-down, exports, and the state kept across the breadcrumb.
import { icon } from "@erpforall/client";
import type { ClientActionEnvironment, ClientActionHandle } from "@erpforall/client";
import * as data from "./report-data.js";
import type { Line, Options } from "./report-data.js";
import { buildFilters, disclosure, message, type Disclosure } from "./report-filters.js";

const MODEL = "reporting.report";
/** The options a returning user gets back: what the user chose, not what the server derived (columns, periods). */
const KEPT = ["companies", "date", "comparison", "unfold_all", "unfolded", "hide_zero", "search", "order", "provider"] as const;

interface Shown { name: string; filters: data.Filters; options: Options; lines: Line[]; computedAt: string; hidden: readonly string[] }

function make<K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

export class ReportScreen implements ClientActionHandle {
  private readonly root = make("section", "report-screen");
  private readonly bar = make("div", "report-bar");
  private readonly status = make("div", "report-status");
  private readonly scroller = make("div", "report-scroll");
  private readonly menu = make("div", "report-menu");
  private readonly buttons = new Map<string, HTMLButtonElement>();
  private shown: Shown | undefined;
  private filtersBar: { dispose(): void } | undefined;
  private caret: { disclosure: Disclosure; trigger: HTMLElement } | undefined;
  private failure: { message: string; retry: () => void } | undefined;
  private phase: "loading" | "ready" | "refreshing" = "loading";
  private readonly busy = new Set<string>();
  private generation = 0;
  private disposed = false;
  private activeId: string | undefined;
  private readonly report: number;
  private readonly lang: string;
  private readonly listeners: (() => void)[] = [];

  constructor(private readonly env: ClientActionEnvironment) {
    const id = env.action.params["report"];
    if (typeof id !== "number" || !Number.isInteger(id) || id < 1) throw new Error("This report action does not name its report.");
    this.report = id; this.lang = env.editorLocale.lang;
    this.root.setAttribute("aria-label", env.action.name);
    this.status.setAttribute("role", "status");
    this.menu.hidden = true;
    this.scroller.addEventListener("click", event => this.onClick(event));
    this.scroller.addEventListener("keydown", event => this.onKey(event));
    this.root.append(this.bar, this.status, this.scroller, this.menu);
    env.host.append(this.root);
    this.buildControls();
    this.render();
    void this.start();
  }

  // ---- calls ----
  private call(method: string, args: Record<string, unknown>): Promise<unknown> { return this.env.rpc.call(MODEL, method, args); }
  private async open(previous: Options | false): Promise<void> {
    const gen = ++this.generation;
    const resolved = data.decodeOptions(await this.call("erp_report_options", { report: this.report, previous }));
    const loaded = data.decodeLines(await this.call("erp_report_lines", { options: resolved.options }));
    if (gen !== this.generation || this.disposed) return;
    this.commit({ name: resolved.name, filters: resolved.filters, ...loaded });
  }
  private async start(): Promise<void> {
    this.phase = "loading"; this.failure = undefined; this.render();
    const restored = this.env.restored;
    const saved = restored && typeof restored === "object" ? (restored as Record<string, unknown>) : null;
    const previous = saved && typeof saved["options"] === "object" && saved["options"] !== null ? saved["options"] as Options : false;
    try { await this.open(previous); }
    catch (error) {
      if (this.disposed) return;
      if (previous === false) { this.fail(error, () => void this.start()); return; }
      this.env.notice(`The previous options of this report could not be restored (${message(error)}) The defaults are shown.`);
      try { await this.open(false); } catch (second) { if (!this.disposed) this.fail(second, () => void this.start()); return; }
    }
    if (typeof saved?.["scroll"] === "number") this.scroller.scrollTop = saved["scroll"];
  }
  /** The report with `options`; the screen keeps its table (dimmed) until the answer arrives, and keeps it if the answer is an error. */
  private reload(options: Options): void {
    const gen = ++this.generation;
    this.phase = this.shown ? "refreshing" : "loading"; this.failure = undefined; this.closeCaret(false); this.renderStatus(); this.syncControls();
    this.scroller.dataset["dim"] = String(this.shown !== undefined);
    void this.call("erp_report_lines", { options }).then(raw => {
      if (gen !== this.generation || this.disposed || !this.shown) return;
      this.commit({ ...this.shown, ...data.decodeLines(raw) });
    }).catch((error: unknown) => {
      if (gen === this.generation && !this.disposed) this.fail(error, () => this.reload(options));
    });
  }
  private commit(next: Shown): void {
    this.shown = next; this.failure = undefined; this.phase = "ready"; this.busy.clear();
    this.render();
  }
  private fail(error: unknown, retry: () => void): void {
    this.failure = { message: message(error), retry }; this.phase = "ready"; this.render();
  }
  private patch(patch: Options): void {
    if (!this.shown) return;
    this.reload({ ...this.shown.options, ...patch });
  }

  // ---- state ----
  captureState(): unknown {
    if (!this.shown) return undefined;
    const options: Options = {};
    for (const key of KEPT) if (key in this.shown.options) options[key] = this.shown.options[key];
    return { options, scroll: this.scroller.scrollTop };
  }
  dispose(): void {
    if (this.disposed) return;
    this.disposed = true; this.generation++;
    this.filtersBar?.dispose(); this.caret?.disclosure.dispose(); this.listeners.forEach(run => run());
    this.root.remove(); this.env.controls.replaceChildren();
  }

  // ---- controls (the control panel's actions slot) ----
  private buildControls(): void {
    const add = (key: string, text: string, iconName: Parameters<typeof icon>[0] | null, run: () => void): void => {
      const node = make("button", "erp-button report-button"); node.type = "button"; node.dataset["action"] = key;
      if (iconName) node.append(icon(iconName));
      node.append(text); node.addEventListener("click", run); this.buttons.set(key, node); this.env.controls.append(node);
    };
    add("refresh", "Refresh", "refresh-cw", () => { if (this.shown) this.reload(this.shown.options); });
    this.syncControls();
  }
  private syncControls(): void {
    const blocked = this.phase !== "ready" || this.shown === undefined && !this.failure;
    for (const node of this.buttons.values()) node.disabled = blocked;
  }

  // ---- rendering ----
  private render(): void {
    this.renderBar(); this.renderStatus(); this.renderTable(); this.syncControls();
  }
  private renderBar(): void {
    this.filtersBar?.dispose(); this.filtersBar = undefined; this.bar.replaceChildren();
    const s = this.shown;
    if (!s) return;
    const built = buildFilters({ env: this.env, options: s.options, filters: s.filters, apply: patch => this.patch(patch) });
    this.filtersBar = built; this.bar.append(built.node);
  }
  private renderStatus(): void {
    this.status.replaceChildren(); this.root.dataset["phase"] = this.failure ? "error" : this.phase;
    if (this.failure) {
      const retry = make("button", "erp-button report-button", "Retry"); retry.type = "button"; retry.addEventListener("click", this.failure.retry);
      const text = make("span", "report-error", this.failure.message); text.setAttribute("role", "alert");
      this.status.append(text, retry); return;
    }
    const s = this.shown;
    if (!s) { this.status.append(make("span", "report-note", "Loading report")); return; }
    if (this.phase === "refreshing") this.status.append(make("span", "report-note", "Refreshing"));
    else this.status.append(make("span", "report-note", `Computed ${s.computedAt}`));
    if (s.hidden.length) this.status.append(make("span", "report-note", `Not shown, no access: ${s.hidden.join(", ")}`));
  }
  private renderTable(): void {
    this.closeCaret(false);
    const focused = this.scroller.contains(document.activeElement) ? (document.activeElement as HTMLElement).closest<HTMLElement>("[data-id]")?.dataset["id"] : undefined;
    this.scroller.replaceChildren();
    const s = this.shown;
    if (!s) {
      if (this.failure) return;
      const skeleton = make("div", "report-skeleton"); skeleton.setAttribute("aria-hidden", "true");
      for (let i = 0; i < 6; i++) skeleton.append(make("div", "report-skeleton-row"));
      this.scroller.append(skeleton); return;
    }
    this.scroller.dataset["dim"] = String(this.phase === "refreshing");
    if (!s.lines.length) { this.scroller.append(make("p", "report-empty", "No data for these filters")); return; }
    const cols = data.columns(s.options), groups = data.periods(s.options);
    const perGroup = groups.length ? cols.length / groups.length : cols.length;
    const table = make("table", "report-table"); table.setAttribute("role", "treegrid"); table.setAttribute("aria-label", s.name);
    const head = make("thead");
    if (groups.length > 1) {
      const row = make("tr"); row.append(make("th", "report-corner"));
      for (const group of groups) {
        const th = make("th", "report-period", group.from ? `${data.formatDate(group.from, this.lang)} – ${data.formatDate(group.to, this.lang)}` : data.formatDate(group.to, this.lang));
        th.colSpan = perGroup; th.scope = "colgroup"; row.append(th);
      }
      head.append(row);
    }
    const names = make("tr"); names.dataset["names"] = "true"; names.append(make("th", "report-name-head", "Line"));
    const comparison = s.options["comparison"], ascending = typeof comparison === "object" && comparison !== null && (comparison as Record<string, unknown>)["order"] === "ascending";
    const main = ascending ? groups.length - 1 : 0, ordered = s.options["order"];
    const order = typeof ordered === "object" && ordered !== null ? ordered as { column?: unknown; direction?: unknown } : null;
    for (const col of cols) {
      const th = make("th", "report-col"); th.scope = "col";
      const sortable = col.sortable && col.group === main;
      const active = sortable && order?.column === col.label;
      if (sortable) {
        th.setAttribute("aria-sort", active ? (order?.direction === "desc" ? "descending" : "ascending") : "none");
        const sort = make("button", "report-sort", col.name); sort.type = "button"; sort.dataset["sort"] = col.label;
        if (active) sort.append(icon("chevron-down"));
        th.append(sort);
      } else th.textContent = col.name;
      names.append(th);
    }
    head.append(names); table.append(head);
    const body = make("tbody");
    const active = s.lines.some(l => l.id === this.activeId) ? this.activeId : s.lines[0]?.id;
    this.activeId = active;
    const frag = document.createDocumentFragment();
    for (const line of s.lines) frag.append(this.row(line, cols.length, line.id === active));
    body.append(frag); table.append(body); this.scroller.append(table);
    if (focused) [...this.scroller.querySelectorAll<HTMLElement>("tr[data-id]")].find(r => r.dataset["id"] === focused)?.focus();
  }
  private row(line: Line, columnCount: number, tab: boolean): HTMLTableRowElement {
    const tr = make("tr", `report-line report-line-${line.kind}`);
    tr.dataset["id"] = line.id; tr.tabIndex = tab ? 0 : -1; tr.setAttribute("role", "row");
    tr.setAttribute("aria-level", String(line.level + 1)); tr.style.setProperty("--level", String(line.level));
    if (line.kind === "more") {
      const td = make("td"); td.colSpan = columnCount + 1;
      const more = make("button", "report-more", `Show more (${line.remaining ?? 0} remaining)`); more.type = "button"; more.dataset["action"] = "more";
      more.disabled = this.busy.has(line.id); td.append(more); tr.append(td); return tr;
    }
    if (line.unfoldable) tr.setAttribute("aria-expanded", String(line.unfolded));
    const name = make("th", "report-name"); name.scope = "row"; name.setAttribute("role", "rowheader");
    const toggle = make("button", "report-toggle"); toggle.type = "button"; toggle.tabIndex = -1; toggle.dataset["action"] = "toggle";
    if (line.unfoldable) {
      toggle.append(icon(line.unfolded ? "chevron-down" : "chevron-right"));
      toggle.setAttribute("aria-label", `${line.unfolded ? "Fold" : "Unfold"} ${line.name}`);
      if (this.busy.has(line.id)) { toggle.disabled = true; tr.setAttribute("aria-busy", "true"); }
    } else toggle.hidden = true;
    const inner = make("div", "report-name-inner"); inner.append(toggle, make("span", "report-label", line.name));
    if (line.kind !== "total") {
      const caret = make("button", "report-caret"); caret.type = "button"; caret.tabIndex = -1; caret.dataset["action"] = "caret";
      caret.append(icon("chevron-down")); caret.setAttribute("aria-label", `Actions of ${line.name}`); inner.append(caret);
    }
    name.append(inner); tr.append(name);
    line.cells.forEach((cell, index) => {
      const td = make("td", `report-cell report-cell-${cell.kind}${cell.zero ? " report-zero" : ""}`, data.formatCell(cell, this.lang));
      td.dataset["col"] = String(index); td.setAttribute("role", "gridcell");
      if (line.kind !== "total" && cell.kind !== "empty" && cell.kind !== "text") td.dataset["drill"] = "true";
      tr.append(td);
    });
    return tr;
  }

  // ---- interaction ----
  private line(id: string): Line | undefined { return this.shown?.lines.find(l => l.id === id); }
  private onClick(event: MouseEvent): void {
    const target = event.target;
    if (!(target instanceof Element)) return;
    const tr = target.closest<HTMLElement>("tr[data-id]"), id = tr?.dataset["id"];
    const sort = target.closest<HTMLElement>("[data-sort]");
    if (sort) { this.sortBy(sort.dataset["sort"] ?? ""); return; }
    if (!id) return;
    this.activeId = id;
    const action = target.closest<HTMLElement>("[data-action]")?.dataset["action"];
    const cell = target.closest<HTMLElement>("td[data-drill]");
    if (action === "toggle") this.toggle(id);
    else if (action === "more") this.loadMore(id);
    else if (action === "caret") this.openCaret(id, target.closest("button") as HTMLElement);
    else if (cell) this.drill(id, { cell: Number(cell.dataset["col"]) });
  }
  private onKey(event: KeyboardEvent): void {
    const tr = (event.target as Element).closest<HTMLElement>("tr[data-id]");
    if (!tr || event.target !== tr || event.altKey && event.key !== "ArrowDown" || event.ctrlKey || event.metaKey) return;
    const id = tr.dataset["id"] ?? "", line = this.line(id);
    if (!line) return;
    const rows = [...this.scroller.querySelectorAll<HTMLElement>("tr[data-id]")], at = rows.indexOf(tr);
    const go = (row: HTMLElement | undefined): void => { if (!row) return; this.activeId = row.dataset["id"]; for (const r of rows) r.tabIndex = r === row ? 0 : -1; row.focus(); };
    const handled = ((): boolean => {
      switch (event.key) {
        case "ArrowDown": if (event.altKey) { this.openCaret(id, tr.querySelector<HTMLElement>(".report-caret")); return true; } go(rows[at + 1]); return true;
        case "ArrowUp": go(rows[at - 1]); return true;
        case "Home": go(rows[0]); return true;
        case "End": go(rows[rows.length - 1]); return true;
        case "ArrowRight": if (line.unfoldable && !line.unfolded) this.toggle(id); else if (line.unfolded) go(rows[at + 1]); return true;
        case "ArrowLeft": if (line.unfolded) this.toggle(id); else go(rows.find(r => r.dataset["id"] === line.parent)); return true;
        case "Enter":
          if (line.kind === "more") this.loadMore(id);
          else if (line.caret[0]) this.drill(id, { item: line.caret[0] });
          else if (line.unfoldable) this.toggle(id);
          return true;
        case "ContextMenu": this.openCaret(id, tr.querySelector<HTMLElement>(".report-caret")); return true;
        case "F10": if (!event.shiftKey) return false; this.openCaret(id, tr.querySelector<HTMLElement>(".report-caret")); return true;
        default: return false;
      }
    })();
    if (handled) event.preventDefault();
  }
  private sortBy(label: string): void {
    if (this.shown) this.patch({ order: data.nextOrder(this.shown.options, label) });
  }
  private toggle(id: string): void {
    const s = this.shown, line = this.line(id);
    if (!s || !line || !line.unfoldable || this.busy.has(id)) return;
    if (line.unfolded) {
      // Folding while "unfold all" is on keeps what else is open as an explicit list, so a refresh does not reopen everything.
      const open = s.options["unfold_all"] === true ? s.lines.filter(l => l.unfolded).map(l => l.id) : data.unfolded(s.options);
      const folded = data.fold(s.lines, open, id);
      this.shown = { ...s, lines: folded.lines, options: { ...s.options, unfold_all: false, unfolded: folded.unfolded } }; this.renderTable(); return;
    }
    const gen = this.generation;
    this.env.run(async () => {
      this.busy.add(id); this.renderTable();
      try {
        const children = data.decodeExpand(await this.call("erp_report_expand", { options: s.options, line_id: id, offset: 0 }));
        if (gen !== this.generation || this.disposed || !this.shown) return;
        this.busy.delete(id);
        const next = data.unfold(this.shown.lines, data.unfolded(this.shown.options), id, children);
        this.shown = { ...this.shown, lines: next.lines, options: { ...this.shown.options, unfolded: next.unfolded } };
        // A total row follows the children of an unfolded line (`totals_below`): the server adds it, so reload the tree.
        if (this.shown.filters.totalsBelow) this.reload(this.shown.options); else this.renderTable();
      } catch (error) {
        if (gen === this.generation && !this.disposed) { this.busy.delete(id); this.renderTable(); }
        throw error;
      }
    });
  }
  private loadMore(id: string): void {
    const s = this.shown, line = this.line(id);
    if (!s || !line || line.kind !== "more" || line.parent === null || line.nextOffset === null || this.busy.has(id)) return;
    const parent = line.parent, offset = line.nextOffset;
    const gen = this.generation;
    this.env.run(async () => {
      this.busy.add(id); this.renderTable();
      try {
        const page = data.decodeExpand(await this.call("erp_report_expand", { options: s.options, line_id: parent, offset }));
        if (gen !== this.generation || this.disposed || !this.shown) return;
        this.busy.delete(id);
        this.shown = { ...this.shown, lines: data.loadMore(this.shown.lines, id, page) }; this.renderTable();
      } catch (error) {
        if (gen === this.generation && !this.disposed) { this.busy.delete(id); this.renderTable(); }
        throw error;
      }
    });
  }
  private drill(id: string, target: { item: string } | { cell: number }): void {
    const s = this.shown;
    if (!s) return;
    this.closeCaret(false);
    this.env.run(async () => { await this.env.open(await this.call("erp_report_drill", { options: s.options, line_id: id, target })); });
  }

  // ---- caret menu ----
  private closeCaret(restore: boolean): void { this.caret?.disclosure.close(restore); }
  private openCaret(id: string, trigger: HTMLElement | null): void {
    const s = this.shown, line = this.line(id);
    if (!s || !line || !trigger) return;
    if (this.caret?.disclosure.isOpen() && this.caret.trigger === trigger) { this.closeCaret(true); return; }
    this.caret?.disclosure.dispose(); this.menu.replaceChildren();
    const items: { text: string; target: { item: string } | { cell: number } }[] = line.caret.map(item => ({ text: item === "open" ? "Open" : item, target: { item } }));
    const cols = data.columns(s.options), main = cols.filter(c => c.group === cols[0]?.group);
    if (line.kind !== "total") main.forEach((col, index) => { if (line.cells[index]?.kind !== "empty") items.push({ text: `Records behind ${col.name}`, target: { cell: index } }); });
    if (!items.length) return;
    for (const item of items) {
      const node = make("button", "report-menu-item", item.text); node.type = "button"; node.setAttribute("role", "menuitem"); node.tabIndex = -1;
      node.addEventListener("click", () => this.drill(id, item.target)); this.menu.append(node);
    }
    this.menu.setAttribute("aria-label", `Actions of ${line.name}`);
    const rect = trigger.getBoundingClientRect(), base = this.root.getBoundingClientRect();
    this.menu.style.top = `${rect.bottom - base.top}px`; this.menu.style.left = `${Math.max(0, rect.left - base.left)}px`;
    const owner = disclosure(trigger, this.menu, "menu", false);
    this.menu.addEventListener("keydown", this.menuKey);
    this.caret = { disclosure: owner, trigger }; owner.open();
    (this.menu.querySelector<HTMLElement>("[role=menuitem]"))?.focus();
  }
  private readonly menuKey = (event: KeyboardEvent): void => {
    const items = [...this.menu.querySelectorAll<HTMLElement>("[role=menuitem]")], at = items.indexOf(document.activeElement as HTMLElement);
    const move = (to: number): void => { event.preventDefault(); items[(to + items.length) % items.length]?.focus(); };
    if (event.key === "ArrowDown") move(at + 1); else if (event.key === "ArrowUp") move(at - 1);
    else if (event.key === "Home") move(0); else if (event.key === "End") move(items.length - 1);
  };
}
