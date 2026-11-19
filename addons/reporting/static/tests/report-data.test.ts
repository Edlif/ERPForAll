/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test } from "vitest";
import * as data from "../src/report-data.js";
import { cell, groupId, lineId, linesAnswer, wireFilters, wireLine, wireOptions } from "./support.js";

const decoded = (rows: Record<string, unknown>[]): data.Line[] => data.decodeLines(linesAnswer(rows)).lines;
const root = lineId("PARTNERS");
const children = (): Record<string, unknown>[] => [
  wireLine(groupId(root, "country_id", "res.country", 1), "Belgium", { kind: "detail" }),
  wireLine(groupId(root, "country_id", "res.country", 2), "France", { kind: "detail" })
];

/// (partial: the client half, children follow the unfolded line and the id joins the unfolded set)
test("unfolding puts the children right after the line and remembers the id once", () => {
  const lines = decoded([wireLine(root, "Contacts", { unfoldable: true }), wireLine(lineId("OTHER"), "Other")]);
  const done = data.unfold(lines, [], root, data.decodeExpand({ lines: children() }));
  expect(done.lines.map(l => l.name)).toEqual(["Contacts", "Belgium", "France", "Other"]);
  expect(done.lines[0]?.unfolded).toBe(true);
  expect(data.unfold(done.lines, done.unfolded, root, []).unfolded).toEqual([root]);
});

/// (partial: the client half, folding drops the descendants and their unfolded ids, never a sibling)
test("folding drops the descendants, their totals and their unfolded ids only", () => {
  const belgium = groupId(root, "country_id", "res.country", 1);
  const lines = decoded([wireLine(root, "Contacts", { unfoldable: true, unfolded: true }), ...children(), wireLine(`${belgium}|total~~`, "Belgium", { kind: "total" }), wireLine(lineId("SIBLING"), "Sibling")]);
  const folded = data.fold(lines, [root, belgium], root);
  expect(folded.lines.map(l => l.name)).toEqual(["Contacts", "Sibling"]);
  expect(folded.lines[0]?.unfolded).toBe(false);
  expect(folded.unfolded).toEqual([]);
  expect(() => data.fold(folded.lines, [], "root~reporting.report~7|line:GONE~~")).toThrow("no longer in the report");
});

/// (partial: the client half, the next page replaces the load-more row)
test("a loaded page replaces the load-more row and only that row", () => {
  const more = wireLine(`${root}|more~~`, "More", { kind: "more", remaining: 5, next_offset: 2 });
  const lines = decoded([wireLine(root, "Contacts", { unfolded: true }), ...children(), more]);
  const next = data.loadMore(lines, `${root}|more~~`, decoded([wireLine(groupId(root, "country_id", "res.country", 3), "Italy", { kind: "detail" })]));
  expect(next.map(l => l.name)).toEqual(["Contacts", "Belgium", "France", "Italy"]);
  expect(() => data.loadMore(next, root, [])).toThrow("no longer in the report");
});

/// (partial: the client half, a click cycles none, descending, ascending, none)
test("a header click cycles descending, ascending, none", () => {
  let options = wireOptions();
  const seen: unknown[] = [];
  for (let i = 0; i < 3; i++) {
    const next = data.nextOrder(options, "count");
    seen.push(next);
    options = { ...options, order: next };
  }
  expect(seen).toEqual([{ column: "count", direction: "desc" }, { column: "count", direction: "asc" }, null]);
  expect(data.nextOrder({ ...options, order: { column: "other", direction: "asc" } }, "count")).toEqual({ column: "count", direction: "desc" });
});

/// (partial: the client half, decimals are formatted from their string, never through a binary float)
test("decimal strings keep every digit in the user's locale", () => {
  const big = data.decodeLines(linesAnswer([wireLine(root, "Big", { cells: [cell("12345678901234567.89", "monetary", 2), cell("-0.50", "percent", 2), cell("1234", "integer"), cell(null, "empty")] })])).lines[0]!.cells;
  const text = big.map(c => data.formatCell(c, "en_US"));
  expect(text).toEqual(["12,345,678,901,234,567.89", "-0.50 %", "1,234", ""]);
  expect(data.formatCell(big[2]!, "de_DE")).toBe("1.234");
  expect(data.formatCell(big[0]!, "not a locale")).toContain("567.89");
});

test("an answer in an unknown form is refused by name", () => {
  expect(() => data.decodeLines({ options: {}, lines: [wireLine(root, "x", { kind: "weird" })], computed_at: "t", hidden_lines: [] })).toThrow("unknown kind 'weird'");
  expect(() => data.decodeLines({ options: {}, lines: "no", computed_at: "t", hidden_lines: [] })).toThrow("the lines in an unknown form");
  expect(() => data.decodeOptions({ name: "x", options: {}, filters: { ...wireFilters(), date_mode: "weekly" } })).toThrow("unknown filter modes");
  expect(() => data.columns({ columns: [{ group: 0, name: "n", label: "l", kind: "other", sortable: false }] })).toThrow("unknown kind 'other'");
});

test("dates are written in the user's locale and an unreadable one is left as sent", () => {
  expect(data.formatDate("2026-11-19", "en_US")).toContain("2026");
  expect(data.formatDate("not-a-date", "en_US")).toBe("not-a-date");
});
