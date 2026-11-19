/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// Builders of the wire answers of `reporting.report` (the shapes of addons/reporting/src/line.rs and options.rs)
// and a client action environment whose RPC is scripted by method name.
import { vi } from "vitest";
import { MessageServices } from "../../../web/static/src/client/messages.js";
import type { ClientActionEnvironment } from "@erpforall/client";

export const REPORT = 7;
export const rootId = `root~reporting.report~${REPORT}`;
export const lineId = (code: string): string => `${rootId}|line:${code}~~`;
export const groupId = (parent: string, field: string, model: string, value: number): string => `${parent}|group:${field}~${model}~${value}`;

export const cell = (value: string | null, kind = "integer", scale = 0, zero = false): Record<string, unknown> => ({ kind, value, scale, currency: null, zero });
export function wireLine(id: string, name: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  const level = id.split("|").length - 2;
  const parent = level > 0 ? id.slice(0, id.lastIndexOf("|")) : null;
  return { id, parent, name, level, kind: "section", unfoldable: false, unfolded: false, cells: [cell("3")], caret: [], remaining: null, next_offset: null, ...over };
}
export function wireOptions(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    report: REPORT, companies: [1], date: { mode: "range", preset: "this_year", from: "2026-01-01", to: "2026-12-31" },
    comparison: { mode: "none", periods: 1, order: "descending", from: null, to: null }, unfold_all: false, unfolded: [], hide_zero: false, search: "", provider: {},
    column_groups: [{ from: "2026-01-01", to: "2026-12-31" }], columns: [{ group: 0, name: "Contacts", label: "count", kind: "integer", sortable: true }], order: null, ...over
  };
}
export const wireFilters = (over: Record<string, unknown> = {}): Record<string, unknown> => ({
  date_mode: "range", comparison: true, multi_company: true, unfold_all: true, hide_zero: "optional", search_bar: true, totals_below: false, ...over
});
export const optionsAnswer = (options = wireOptions(), filters = wireFilters()): Record<string, unknown> => ({ name: "Contacts by Country", options, filters });
export const linesAnswer = (lines: Record<string, unknown>[], options = wireOptions()): Record<string, unknown> => ({ options, lines, computed_at: "2026-11-19 10:00:00", hidden_lines: [] });

export type Handler = (args: Record<string, unknown>) => unknown;
export interface Rig { readonly env: ClientActionEnvironment; readonly calls: { method: string; args: Record<string, unknown> }[]; readonly errors: unknown[]; readonly open: ReturnType<typeof vi.fn>; readonly notice: ReturnType<typeof vi.fn>; dispose(): void }

export function rig(handlers: Record<string, Handler>, params: Record<string, unknown> = { report: REPORT }, restored: unknown = null): Rig {
  const host = document.createElement("div"), controls = document.createElement("div"), panel = document.createElement("div");
  document.body.append(host, controls);
  const calls: Rig["calls"] = [], errors: unknown[] = [];
  const messages = new MessageServices(host, error => errors.push(error));
  const open = vi.fn(async () => undefined), notice = vi.fn();
  const rpc = { call: async (model: string, method: string, args: Record<string, unknown>) => {
    if (model !== "reporting.report") throw new Error(`unexpected model ${model}`);
    calls.push({ method, args });
    const handler = handlers[method];
    if (!handler) throw new Error(`unexpected method ${method}`);
    return handler(args);
  } };
  const env = {
    host, panel, controls, rpc, companies: [{ id: 1, name: "Arkana" }, { id: 2, name: "Beta" }], run: (job: () => Promise<void>) => { void job().catch(error => errors.push(error)); },
    notice, messages, editorLocale: { lang: "en_US", tz: "UTC", date_format: "%m/%d/%Y", time_format: "%H:%M:%S" },
    action: { id: 5, name: "Contacts by Country", tag: "reporting.report", params }, restored, open
  } as unknown as ClientActionEnvironment;
  return { env, calls, errors, open, notice, dispose: () => { messages.dispose(); host.remove(); controls.remove(); } };
}
export const settle = async (): Promise<void> => { for (let i = 0; i < 6; i++) await new Promise(resolve => setTimeout(resolve, 0)); };
