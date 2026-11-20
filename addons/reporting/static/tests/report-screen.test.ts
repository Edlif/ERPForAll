/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, expect, test, vi } from "vitest";
import { ReportScreen } from "../src/report-screen.js";
import { auditComponent } from "../../../../web/testing/axe.js";
import { cell, groupId, lineId, linesAnswer, optionsAnswer, REPORT, rig, settle, wireFilters, wireLine, wireOptions, type Rig } from "./support.js";

const root = lineId("PARTNERS");
const belgium = groupId(root, "country_id", "res.country", 1), france = groupId(root, "country_id", "res.country", 2);
const top = (over: Record<string, unknown> = {}): Record<string, unknown> => wireLine(root, "Contacts", { unfoldable: true, caret: [], cells: [cell("9")], ...over });
const country = (id: string, name: string, count: string): Record<string, unknown> => wireLine(id, name, { kind: "detail", cells: [cell(count)], caret: ["open"] });

let current: Rig | undefined;
let screen: ReportScreen | undefined;
afterEach(() => { screen?.dispose(); current?.dispose(); screen = current = undefined; vi.restoreAllMocks(); });

function mount(handlers: Parameters<typeof rig>[0], params?: Record<string, unknown>, restored?: unknown): Rig {
  current = rig(handlers, params, restored);
  screen = new ReportScreen(current.env);
  return current;
}
const rows = (r: Rig): HTMLElement[] => [...r.env.host.querySelectorAll<HTMLElement>("tr.report-line")];
const names = (r: Rig): string[] => rows(r).map(row => row.querySelector(".report-label")?.textContent ?? row.textContent ?? "");
const standard = (extra: Parameters<typeof rig>[0] = {}): Parameters<typeof rig>[0] => ({
  erp_report_options: () => optionsAnswer(),
  erp_report_lines: () => linesAnswer([top()]),
  erp_report_expand: () => ({ lines: [country(belgium, "Belgium", "4"), country(france, "France", "5")], computed_at: "t" }),
  ...extra
});
const click = (node: Element | null | undefined): void => { if (!node) throw new Error("nothing to click"); node.dispatchEvent(new MouseEvent("click", { bubbles: true })); };

/// (partial: the screen asks for the options of its report first, with no previous options, then for the lines of what came back)
test("the first opening asks for options, then lines, and shows the table", async () => {
  const r = mount(standard());
  expect(r.env.host.querySelector(".report-skeleton")).not.toBeNull();
  await settle();
  expect(r.calls.map(c => c.method)).toEqual(["erp_report_options", "erp_report_lines"]);
  expect(r.calls[0]?.args).toEqual({ report: REPORT, previous: false });
  expect(r.calls[1]?.args).toEqual({ options: wireOptions() });
  expect(names(r)).toEqual(["Contacts"]);
  expect(rows(r)[0]?.querySelector(".report-cell")?.textContent).toBe("9");
  expect(r.env.host.querySelector("table")?.getAttribute("role")).toBe("treegrid");
  expect(r.env.host.querySelector(".report-status")?.textContent).toContain("Computed 2026-11-19 10:00:00");
});

/// (partial: one period row above the column row when comparison gives several column groups, the main period sorts)
test("comparison column groups get a period row and only the main period sorts", async () => {
  const options = wireOptions({
    column_groups: [{ from: "2026-01-01", to: "2026-12-31" }, { from: "2025-01-01", to: "2025-12-31" }],
    columns: [{ group: 0, name: "Contacts", label: "count", kind: "integer", sortable: true }, { group: 1, name: "Contacts", label: "count", kind: "integer", sortable: true }]
  });
  const r = mount(standard({ erp_report_options: () => optionsAnswer(options), erp_report_lines: () => linesAnswer([top({ cells: [cell("9"), cell("7")] })], options) }));
  await settle();
  expect(r.env.host.querySelectorAll(".report-period")).toHaveLength(2);
  expect(r.env.host.querySelectorAll(".report-sort")).toHaveLength(1);
  expect(r.env.host.querySelectorAll("tbody .report-cell")).toHaveLength(2);
});

test("an empty answer says so and a failed one shows the server's message with Retry", async () => {
  let fail = true;
  const r = mount(standard({ erp_report_lines: () => { if (fail) throw new Error("Access denied to res.partner."); return linesAnswer([]); } }));
  await settle();
  expect(r.env.host.querySelector(".report-error")?.textContent).toBe("Access denied to res.partner.");
  expect(r.env.host.querySelector(".report-error")?.getAttribute("role")).toBe("alert");
  fail = false;
  click([...r.env.host.querySelectorAll("button")].find(b => b.textContent === "Retry"));
  await settle();
  expect(r.env.host.querySelector(".report-empty")?.textContent).toBe("No data for these filters");
});

test("Refresh retries a first load that failed", async () => {
  let fail = true;
  const r = mount(standard({ erp_report_lines: () => { if (fail) throw new Error("The server is busy."); return linesAnswer([top()]); } }));
  await settle();
  expect(r.env.host.querySelector(".report-error")?.textContent).toBe("The server is busy.");
  fail = false;
  click(r.env.host.querySelector("[data-action=refresh]"));
  await settle();
  expect(names(r)).toEqual(["Contacts"]);
});

/// (partial: a click unfolds through erp_report_expand at offset 0 and the children appear below the line)
test("unfolding loads the children once and ignores a second request while loading", async () => {
  let release: (() => void) | undefined;
  const r = mount(standard({ erp_report_expand: () => new Promise(resolve => { release = () => resolve({ lines: [country(belgium, "Belgium", "4")], computed_at: "t" }); }) }));
  await settle();
  click(rows(r)[0]?.querySelector(".report-toggle"));
  await settle();
  expect(rows(r)[0]?.getAttribute("aria-busy")).toBe("true");
  rows(r)[0]!.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
  await settle();
  expect(r.calls.filter(c => c.method === "erp_report_expand")).toHaveLength(1);
  release?.();
  await settle();
  expect(names(r)).toEqual(["Contacts", "Belgium"]);
  expect(r.calls.find(c => c.method === "erp_report_expand")?.args).toEqual({ options: wireOptions(), line_id: root, offset: 0 });
  expect(rows(r)[0]?.getAttribute("aria-expanded")).toBe("true");
  expect(rows(r)[1]?.getAttribute("aria-level")).toBe("2");
});

/// (partial: folding is local, no request)
/// (partial: unfolded lines travel in the options of the next request, folded ones do not)
test("folding is local; the next request carries only the lines still unfolded", async () => {
  const r = mount(standard({ erp_report_lines: args => {
    const options = args["options"] as Record<string, unknown>, open = (options["unfolded"] as string[]).includes(root);
    return linesAnswer(open ? [top({ unfolded: true }), country(belgium, "Belgium", "4")] : [top()], options);
  } }));
  await settle();
  click(rows(r)[0]?.querySelector(".report-toggle"));
  await settle();
  expect(names(r)).toEqual(["Contacts", "Belgium", "France"]);
  const before = r.calls.length;
  click(rows(r)[0]?.querySelector(".report-toggle"));
  expect(names(r)).toEqual(["Contacts"]);
  expect(r.calls).toHaveLength(before);
  click(r.env.host.querySelector("[data-action=refresh]"));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ unfolded: [] }) });
  click(rows(r)[0]?.querySelector(".report-toggle"));
  await settle();
  const search = r.env.host.querySelector<HTMLInputElement>("input[name=search]")!;
  search.value = "bel";
  search.form?.dispatchEvent(new Event("submit", { cancelable: true }));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ unfolded: [root], search: "bel" }) });
});

/// (partial: Show more fetches the parent's next page at the row's own offset)
test("load more asks for the next page of the parent at the offset the row names", async () => {
  const more = wireLine(`${root}|more~~`, "More", { kind: "more", remaining: 3, next_offset: 2 });
  const r = mount(standard({
    erp_report_lines: () => linesAnswer([top({ unfolded: true }), country(belgium, "Belgium", "4"), country(france, "France", "5"), more]),
    erp_report_expand: () => ({ lines: [country(groupId(root, "country_id", "res.country", 3), "Italy", "6")], computed_at: "t" })
  }));
  await settle();
  expect(r.env.host.querySelector(".report-more")?.textContent).toBe("Show more (3 remaining)");
  click(r.env.host.querySelector(".report-more"));
  await settle();
  expect(r.calls.find(c => c.method === "erp_report_expand")?.args).toEqual({ options: wireOptions(), line_id: root, offset: 2 });
  expect(names(r)).toEqual(["Contacts", "Belgium", "France", "Italy"]);
  expect(r.env.host.querySelector(".report-more")).toBeNull();
});

/// (partial: a header click sends the order and the answer's order marks the header)
test("sorting sends the order and the header shows it", async () => {
  const r = mount(standard({ erp_report_lines: args => linesAnswer([top()], args["options"] as Record<string, unknown>) }));
  await settle();
  click(r.env.host.querySelector(".report-sort"));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ order: { column: "count", direction: "desc" } }) });
  expect(r.env.host.querySelector(".report-col")?.getAttribute("aria-sort")).toBe("descending");
  click(r.env.host.querySelector(".report-sort"));
  await settle();
  expect(r.env.host.querySelector(".report-col")?.getAttribute("aria-sort")).toBe("ascending");
});

/// (partial: a preset in the menu sends the preset and the report's date mode)
test("a date preset is sent with the report's date mode", async () => {
  const r = mount(standard());
  await settle();
  click(r.env.host.querySelector("[data-filter=date] .report-filter-button"));
  const picker = r.env.host.querySelector<HTMLSelectElement>("select[name=preset]")!;
  expect([...picker.options].map(o => o.value)).not.toContain("today");
  picker.value = "last_month";
  picker.dispatchEvent(new Event("change", { bubbles: true }));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ date: { mode: "range", preset: "last_month", from: null, to: null } }) });
});

/// (partial: single-date reports offer Today)
test("a single-date report offers Today", async () => {
  const single = wireOptions({ date: { mode: "single", preset: "today", from: null, to: "2026-11-19" }, column_groups: [{ from: null, to: "2026-11-19" }] });
  const r = mount(standard({ erp_report_options: () => optionsAnswer(single, wireFilters({ date_mode: "single" })), erp_report_lines: () => linesAnswer([top()], single) }));
  await settle();
  click(r.env.host.querySelector("[data-filter=date] .report-filter-button"));
  const picker = r.env.host.querySelector<HTMLSelectElement>("select[name=preset]")!;
  expect([...picker.options].map(o => o.value)).toContain("today");
  expect(picker.value).toBe("today");
});

/// (partial: a custom range typed in the user's date format is sent as year-month-day with the custom preset)
test("a custom range is typed in the user's format and sent as ISO dates", async () => {
  const r = mount(standard());
  await settle();
  click(r.env.host.querySelector("[data-filter=date] .report-filter-button"));
  const picker = r.env.host.querySelector<HTMLSelectElement>("select[name=preset]")!;
  picker.value = "custom"; picker.dispatchEvent(new Event("change", { bubbles: true }));
  expect(r.env.host.querySelector<HTMLElement>("[data-filter=date] .report-custom")?.hidden).toBe(false);
  const [from, to] = [...r.env.host.querySelectorAll<HTMLInputElement>("[data-filter=date] .report-custom input[type=text], [data-filter=date] .report-custom input:not([type])")];
  from!.value = "01/05/2026"; from!.dispatchEvent(new Event("change", { bubbles: true }));
  to!.value = "01/20/2026"; to!.dispatchEvent(new Event("change", { bubbles: true }));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ date: { mode: "range", preset: "custom", from: "2026-01-05", to: "2026-01-20" } }) });
});

/// (partial: with the oldest period first the newest, last group is the one that sorts)
test("with the oldest period first, the last column group is the sortable main period", async () => {
  const options = wireOptions({
    comparison: { mode: "previous_period", periods: 1, order: "ascending", from: null, to: null },
    column_groups: [{ from: "2025-01-01", to: "2025-12-31" }, { from: "2026-01-01", to: "2026-12-31" }],
    columns: [{ group: 0, name: "Contacts", label: "count", kind: "integer", sortable: true }, { group: 1, name: "Contacts", label: "count", kind: "integer", sortable: true }]
  });
  const r = mount(standard({ erp_report_options: () => optionsAnswer(options), erp_report_lines: () => linesAnswer([top({ cells: [cell("7"), cell("9")] })], options) }));
  await settle();
  const heads = [...r.env.host.querySelectorAll("thead tr[data-names] th.report-col")];
  expect(heads.map(h => h.querySelector(".report-sort") !== null)).toEqual([false, true]);
});

/// (partial: with totals below, an unfold reloads the tree so the server adds the total row)
test("with totals below, unfolding reloads the tree with the line unfolded", async () => {
  const r = mount(standard({ erp_report_options: () => optionsAnswer(wireOptions(), wireFilters({ totals_below: true })) }));
  await settle();
  click(rows(r)[0]?.querySelector(".report-toggle"));
  await settle();
  expect(r.calls.at(-1)).toEqual({ method: "erp_report_lines", args: { options: wireOptions({ unfolded: [root] }) } });
});

/// (partial: the comparison menu sends mode, periods and order)
test("a comparison is sent as mode, periods and order", async () => {
  const r = mount(standard());
  await settle();
  click(r.env.host.querySelector("[data-filter=comparison] .report-filter-button"));
  const mode = r.env.host.querySelector<HTMLSelectElement>("select[name=comparison_mode]")!;
  mode.value = "previous_period"; mode.dispatchEvent(new Event("change", { bubbles: true }));
  const periods = r.env.host.querySelector<HTMLInputElement>("input[name=comparison_periods]")!;
  periods.value = "2";
  click([...r.env.host.querySelectorAll("[data-filter=comparison] button")].find(b => b.textContent === "Apply"));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ comparison: { mode: "previous_period", periods: 2, order: "descending" } }) });
});

/// (partial: the companies menu sends the chosen ids and refuses an empty choice)
test("the companies menu sends the chosen ids and refuses none", async () => {
  const r = mount(standard());
  await settle();
  const apply = (): Element | undefined => [...r.env.host.querySelectorAll("[data-filter=companies] button")].find(b => b.textContent === "Apply");
  const boxes = [...r.env.host.querySelectorAll<HTMLInputElement>("[data-filter=companies] input")];
  expect(boxes.map(b => b.checked)).toEqual([true, false]);
  boxes[0]!.checked = false; click(apply());
  expect(r.notice).toHaveBeenCalledWith("Select at least one company.");
  boxes[1]!.checked = true; click(apply());
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ companies: [2] }) });
});

/// (partial: the Options menu offers what the report offers and sends the flag)
/// (partial: hide zero is sent as an option)
test("the Options menu sends unfold all and hide zero, and offers only what the report allows", async () => {
  const r = mount(standard());
  await settle();
  const flags = (): HTMLInputElement[] => [...r.env.host.querySelectorAll<HTMLInputElement>("[data-filter=options] input")];
  expect(flags().map(f => f.name)).toEqual(["unfold_all", "hide_zero"]);
  flags()[1]!.checked = true; flags()[1]!.dispatchEvent(new Event("change", { bubbles: true }));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions({ hide_zero: true }) });
  screen!.dispose(); r.dispose();
  const limited = mount(standard({ erp_report_options: () => optionsAnswer(wireOptions(), wireFilters({ unfold_all: false, hide_zero: "never", comparison: false, multi_company: false, search_bar: false })) }));
  await settle();
  expect(limited.env.host.querySelector("[data-filter=options]")).toBeNull();
  expect(limited.env.host.querySelector("[data-filter=comparison]")).toBeNull();
  expect(limited.env.host.querySelector("input[name=search]")).toBeNull();
});

/// (partial: Enter and the caret menu drill into the record and open the result through the page)
test("Enter and the caret menu drill into the record behind a line", async () => {
  const action = { type: "ir.actions.act_window", res_model: "res.country", res_id: 1 };
  const r = mount(standard({
    erp_report_lines: () => linesAnswer([top({ unfolded: true }), country(belgium, "Belgium", "4")]),
    erp_report_drill: () => action
  }));
  await settle();
  const row = rows(r)[1]!;
  row.focus();
  row.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }));
  await settle();
  expect(r.calls.at(-1)).toEqual({ method: "erp_report_drill", args: { options: wireOptions(), line_id: belgium, target: { item: "open" } } });
  expect(r.open).toHaveBeenCalledWith(action);
  click(row.querySelector("[data-action=caret]"));
  const items = [...r.env.host.querySelectorAll<HTMLElement>("[role=menuitem]")].map(i => i.textContent);
  expect(items).toEqual(["Open", "Records behind Contacts"]);
  click(r.env.host.querySelectorAll<HTMLElement>("[role=menuitem]")[1]);
  await settle();
  expect(r.calls.at(-1)?.args["target"]).toEqual({ cell: 0 });
});

/// (partial: a click on a cell drills with the column's index)
test("clicking a number drills into the records behind that cell", async () => {
  const r = mount(standard({ erp_report_drill: () => ({ type: "ir.actions.act_window", res_model: "res.partner" }) }));
  await settle();
  click(rows(r)[0]?.querySelector("td[data-drill]"));
  await settle();
  expect(r.calls.at(-1)?.args).toEqual({ options: wireOptions(), line_id: root, target: { cell: 0 } });
  expect(r.open).toHaveBeenCalledTimes(1);
});

test("arrow keys move between lines, unfold with Right and fold or go to the parent with Left", async () => {
  const r = mount(standard());
  await settle();
  const press = (key: string): void => { (document.activeElement as HTMLElement).dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true })); };
  rows(r)[0]!.focus();
  press("ArrowRight"); await settle();
  expect(names(r)).toEqual(["Contacts", "Belgium", "France"]);
  press("ArrowDown");
  expect((document.activeElement as HTMLElement).dataset["id"]).toBe(belgium);
  press("ArrowLeft");
  expect((document.activeElement as HTMLElement).dataset["id"]).toBe(root);
  press("ArrowLeft");
  expect(names(r)).toEqual(["Contacts"]);
  expect(document.activeElement).toBe(rows(r)[0]);
  press("End");
  expect(document.activeElement).toBe(rows(r)[0]);
});

/// (partial: the screen exports its options and scroll; a return hands them to the options call)
test("coming back restores the options and falls back to the defaults, saying so, when they no longer apply", async () => {
  const kept = wireOptions({ search: "bel", unfolded: [root] });
  const first = mount(standard({ erp_report_options: args => optionsAnswer(args["previous"] === false ? wireOptions() : kept), erp_report_lines: () => linesAnswer([top()], kept) }));
  await settle();
  const saved = screen!.captureState() as { options: Record<string, unknown>; scroll: number };
  expect(Object.keys(saved.options).sort()).toEqual(["companies", "comparison", "date", "hide_zero", "order", "provider", "search", "unfold_all", "unfolded"]);
  expect(JSON.stringify(saved).length).toBeLessThan(16384);
  screen!.dispose(); first.dispose();

  const back = mount(standard({ erp_report_options: () => optionsAnswer(kept) }), { report: REPORT }, saved);
  await settle();
  expect(back.calls[0]?.args).toEqual({ report: REPORT, previous: saved.options });
  screen!.dispose(); back.dispose();

  let strict = true;
  const stale = mount(standard({ erp_report_lines: () => { if (strict) { strict = false; throw new Error("options.search: this report has no search bar."); } return linesAnswer([top()]); } }), { report: REPORT }, saved);
  await settle();
  expect(stale.notice.mock.calls[0]?.[0]).toContain("options.search: this report has no search bar.");
  expect(names(stale)).toEqual(["Contacts"]);
  expect(stale.calls.filter(c => c.method === "erp_report_options").map(c => c.args["previous"])).toEqual([saved.options, false]);
});

/// (partial: PDF and XLSX post the report id, the current options and the format; the file is saved as received)
/// (partial: only options are posted, never rows)
test("the export buttons post the options and the format", async () => {
  const save = vi.fn();
  const urls = URL as unknown as { createObjectURL: unknown; revokeObjectURL: unknown };
  urls.createObjectURL = () => "blob:test"; urls.revokeObjectURL = () => undefined;
  const link = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(save);
  const r = mount(standard());
  r.postFile.mockResolvedValueOnce({ blob: new Blob(["%PDF"], { type: "application/pdf" }), fileName: "report.pdf" });
  r.postFile.mockResolvedValueOnce({ blob: new Blob(["XLSX fixture"], { type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" }), fileName: "report.xlsx" });
  await settle();
  const button = (action: string): HTMLButtonElement => r.env.host.querySelector<HTMLButtonElement>(`[data-action=${action}]`)!;
  click(button("pdf"));
  await settle();
  expect(r.postFile).toHaveBeenCalledWith("/erp/reporting/export", { report: REPORT, options: wireOptions(), format: "pdf" }, "application/pdf");
  click(button("xlsx"));
  await settle();
  expect(r.postFile.mock.calls[1]).toEqual(["/erp/reporting/export", { report: REPORT, options: wireOptions(), format: "xlsx" }, "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"]);
  expect(link).toHaveBeenCalledTimes(2);
  expect(button("pdf").disabled).toBe(false);
});

test("Print reserves one viewer, falls back when blocked and closes a refused export", async () => {
  const r = mount(standard());
  await settle();
  const viewer = { opener: window, location: { replace: vi.fn() }, close: vi.fn() };
  const open = vi.spyOn(window, "open").mockReturnValue(viewer as unknown as Window);
  const download = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});
  const prior = URL.createObjectURL;
  URL.createObjectURL = () => "blob:report";
  const file = { blob: new Blob(["pdf"]), fileName: "report.pdf" };
  let release!: (value: typeof file) => void;
  r.postFile.mockImplementationOnce(() => new Promise(resolve => { release = resolve; }));
  const print = () => click(r.env.host.querySelector("[data-action=print]"));
  print();
  expect(open).toHaveBeenCalledOnce();
  expect(viewer.opener).toBeNull();
  release(file);
  await settle();
  expect(viewer.location.replace).toHaveBeenCalledWith("blob:report");
  expect(download).not.toHaveBeenCalled();
  open.mockReturnValueOnce(null);
  r.postFile.mockResolvedValueOnce(file);
  print();
  await settle();
  expect(download).toHaveBeenCalledOnce();
  r.postFile.mockRejectedValueOnce(new Error("Export refused"));
  print();
  await settle();
  expect(viewer.close).toHaveBeenCalledOnce();
  expect(r.errors[0]).toEqual(new Error("Export refused"));
  URL.createObjectURL = prior;
});

test("a refused export is reported and the buttons come back", async () => {
  const r = mount(standard());
  r.postFile.mockRejectedValue(new Error("The export is over the row limit (reporting.export_max_rows)."));
  await settle();
  click(r.env.host.querySelector("[data-action=pdf]"));
  await settle();
  expect(r.errors).toHaveLength(1);
  expect((r.errors[0] as Error).message).toContain("reporting.export_max_rows");
  expect(r.env.host.querySelector<HTMLButtonElement>("[data-action=pdf]")?.disabled).toBe(false);
});

test("a screen without a report in its params refuses to open, and a disposed screen ignores late answers", async () => {
  current = rig(standard(), {});
  expect(() => new ReportScreen(current!.env)).toThrow("does not name its report");
  current.dispose();
  let release: (() => void) | undefined;
  const r = mount(standard({ erp_report_lines: () => new Promise(resolve => { release = () => resolve(linesAnswer([top()])); }) }));
  await settle();
  screen!.dispose(); screen!.dispose();
  release?.();
  await settle();
  expect(r.env.host.querySelector("table")).toBeNull();
  expect(r.env.host.children).toHaveLength(0);
});

/// DYNREP accessibility: labels, roles and names of the screen pass the structural axe audit (contrast is audited in the browser script)
test("the screen has no serious structural accessibility violation, open or with a menu out", async () => {
  const r = mount(standard({ erp_report_lines: () => linesAnswer([top({ unfolded: true }), country(belgium, "Belgium", "4")]) }));
  await settle();
  await auditComponent(r.env.host);
  click(r.env.host.querySelector("[data-filter=date] .report-filter-button"));
  click(rows(r)[1]?.querySelector("[data-action=caret]"));
  await auditComponent(r.env.host);
});
