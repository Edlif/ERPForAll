/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// The graph view: one measure drawn as a bar, line or pie chart over one or two groupings. This file holds the pure parts
// (arch and field list to config, the read, the series the chart draws, the address); `graph-view.ts` draws them.
// The read is the pivot's (`formatted_read_grouping_sets`, decoded by `decodePivotAnswer`): each grouping alone and both crossed.
import { ClientError } from "./transport.js";
import type { GraphFieldNode, View } from "./metadata.js";
import { COUNT, decodePivotAnswer, groupingKey, PIVOT_INTERVALS, resolveSpec, specOf, type PivotCall, type PivotField, type PivotInterval, type PivotKind, type PivotMeasure } from "./pivot.js";

export type GraphMode = "bar" | "line" | "pie";
export type GraphOrder = "asc" | "desc";
export const GRAPH_MODES: readonly GraphMode[] = ["bar", "line", "pie"];
/** The first grouping is the axis (or the slices), the second the series: the most the server admits in an arch, and the search's bound here. */
export const GRAPH_MAX_GROUPINGS = 2;
const GROUPING_KINDS: readonly string[] = ["boolean", "char", "integer", "selection", "many2one", "date", "datetime"];
const refuse = (message: string): never => { throw new ClientError("validation", message); };
const invalid = (what: string): never => { throw new ClientError("server", `Invalid ${what}.`); };

export interface GraphConfig {
  mode: GraphMode; stacked: boolean; cumulated: boolean; order: GraphOrder | null; disableLinking: boolean;
  groupBy: readonly PivotField[];
  /** Count, then every summable field of the model by label (the arch's label wins). */
  measures: readonly PivotMeasure[];
  /** What the chart draws first: the arch's last declared measure, or Count (reference `graph_arch_parser.js` keeps the last). */
  measure: string;
  groupable: ReadonlyMap<string, PivotField>;
}
export interface GraphState { mode: GraphMode; stacked: boolean; cumulated: boolean; order: GraphOrder | null; measure: string; groupBy: readonly PivotField[] }

/** A grouping from its `field` or `field:interval` text (the search's Group By, the address). */
export function graphGrouping(spec: string, groupable: ReadonlyMap<string, PivotField>): PivotField {
  if (!groupable.has(spec.split(":")[0]!)) refuse(`The graph cannot group by ${spec}.`);
  return resolveSpec(spec, groupable);
}

/** The graph arch as the server delivered it (`graph` root, `graph_field` children) and the model's field list. */
export function readGraphView(view: View, catalogue: { groupable: ReadonlyMap<string, PivotField>; measures: readonly PivotMeasure[] }): GraphConfig {
  const root = view.nodes[0];
  if (view.type !== "graph" || !root || root.kind !== "graph") return invalid("graph view");
  const groupBy: PivotField[] = [], declared: GraphFieldNode[] = [];
  for (const child of root.children) {
    if (child.kind !== "graph_field") return invalid("graph child");
    if (child.role === "measure") { declared.push(child); continue; }
    const { field } = child;
    if (!GROUPING_KINDS.includes(field.kind)) return invalid(`graph grouping ${field.name}`);
    const dated = field.kind === "date" || field.kind === "datetime";
    if (child.interval !== null && (!dated || !(PIVOT_INTERVALS as readonly string[]).includes(child.interval))) return invalid(`graph interval of ${field.name}`);
    groupBy.push({ name: field.name, label: child.label, kind: field.kind as PivotKind, selection: field.selection, interval: dated ? (child.interval ?? "month") as PivotInterval : null });
  }
  const byId = new Map<string, PivotMeasure>(catalogue.measures.map(measure => [measure.id, measure]));
  for (const node of declared) {
    const kind = node.field.kind;
    if (kind !== "integer" && kind !== "float" && kind !== "monetary") return invalid(`graph measure ${node.field.name}`);
    byId.set(node.field.name, { id: node.field.name, label: node.label, spec: `${node.field.name}:sum`, kind: kind === "integer" ? "integer" : "float" });
  }
  const config: GraphConfig = {
    mode: root.mode, stacked: root.stacked, cumulated: root.cumulated, order: root.order, disableLinking: root.disableLinking, groupBy,
    measures: [COUNT, ...[...byId.values()].sort((a, b) => a.label.localeCompare(b.label))],
    measure: declared.length ? declared[declared.length - 1]!.field.name : COUNT.id, groupable: catalogue.groupable
  };
  check(archState(config), config);
  return config;
}

/** One point of a series: its value (0 where no record falls) and, when records fall there, the domain that opens them. */
export interface GraphPoint { value: number; domain: unknown[] | undefined }
export interface GraphSeries { label: string; points: GraphPoint[] }
export interface GraphSlice { label: string; value: number; domain: unknown[] | undefined; index: number }
export interface GraphData {
  /** The axis labels (bar and line) in the order drawn. */
  labels: string[];
  /** One series per value of the second grouping, or one named after the measure. */
  series: GraphSeries[];
  /** The pie's slices: every point above zero, labelled by its group (and series). */
  slices: GraphSlice[];
  measure: PivotMeasure;
  /** No group holds a record. */
  empty: boolean;
  /** The last read failed: nothing held belongs to the search on screen. */
  unavailable: boolean;
}

type Part = ReturnType<typeof decodePivotAnswer>;
const key = (path: readonly string[]): string => path.join("\u0000");

export class GraphModel {
  private state: GraphState;
  private part: Part | null = null;
  private heldDomain: unknown[] = [];
  private heldMeasure = "";
  private failed = false;
  private tail: Promise<unknown> = Promise.resolve();

  constructor(readonly config: GraphConfig, private readonly call: PivotCall, private readonly currentDomain: () => unknown[], private readonly maxGroups: number, initial: GraphState = archState(config)) {
    this.state = check(initial, config);
  }

  get current(): Readonly<GraphState> { return this.state; }

  /** Read the chart for the search on screen. The state changes only when the read succeeds; reads run one after the other. */
  load(next: GraphState = this.state): Promise<void> {
    const job = this.tail.then(async () => {
      const state = check(next, this.config);
      const domain = this.currentDomain();
      const measure = this.config.measures.find(item => item.id === state.measure)!;
      const aggregates = measure.id === COUNT.id ? [COUNT.spec] : [COUNT.spec, measure.spec];
      const [first, second] = state.groupBy;
      const plans = second ? [{ r: 1, c: 0 }, { r: 0, c: 1 }, { r: 1, c: 1 }] : first ? [{ r: 1, c: 0 }] : [{ r: 0, c: 0 }];
      const axes: [PivotField[], PivotField[]] = [first ? [first] : [], second ? [second] : []];
      const groupingSets = plans.map(plan => [...axes[0].slice(0, plan.r), ...axes[1].slice(0, plan.c)].map(specOf));
      try {
        const raw = await this.call({ groupingSets, aggregates, domain });
        if (Array.isArray(raw)) raw.forEach((rows, index) => {
          if (!Array.isArray(rows) || rows.length <= this.maxGroups) return;
          const names = [...axes[0].slice(0, plans[index]!.r), ...axes[1].slice(0, plans[index]!.c)].map(field => field.label).join(" and ");
          refuse(`Grouping by ${names} gives more than ${this.maxGroups} groups, the most one chart draws (set in Settings: Graph view, most groups). Group by another field or narrow the search.`);
        });
        this.part = decodePivotAnswer(raw, plans, axes, aggregates, { maxGroups: this.maxGroups, maxCells: this.maxGroups, maxOpen: 0 });
      } catch (error) { this.failed = true; throw error; }
      this.heldDomain = domain; this.heldMeasure = measure.id; this.state = state; this.failed = false;
    });
    this.tail = job.catch(() => undefined);
    return job;
  }

  /** Changes that need no read: the chart type, stacking, cumulation and order. */
  setLocal(change: Partial<Pick<GraphState, "mode" | "stacked" | "cumulated" | "order">>): void { this.state = check({ ...this.state, ...change }, this.config); }
  setMeasure(id: string): Promise<void> { return this.load({ ...this.state, measure: id }); }
  /** The search's Group By, or `null` for the arch's groupings. */
  setGroupBy(fields: readonly PivotField[] | null): Promise<void> { return this.load({ ...this.state, groupBy: fields ?? this.config.groupBy }); }

  data(): GraphData {
    const measure = this.config.measures.find(item => item.id === this.heldMeasure) ?? COUNT;
    const empty: GraphData = { labels: [], series: [], slices: [], measure, empty: true, unavailable: this.failed };
    const { part } = this;
    if (!part || this.failed) return empty;
    const { groupBy, mode, order, cumulated } = this.state;
    const total = { key: "", label: "Total", domain: [] as unknown[] };
    // A line draws no point for the records with no value on its axis (reference graph_model.js `_getProcessedDataPoints`).
    const unset = mode === "line" && groupBy.length > 0 && groupBy[0]!.kind !== "boolean";
    const xs = groupBy.length ? (part.heads[0].get("") ?? []).filter(x => !(unset && x.key === "false")) : [total];
    const ss = groupBy.length > 1 ? part.heads[1].get("") ?? [] : [{ ...total, label: measure.label }];
    const point = (x: (typeof xs)[number], s: (typeof ss)[number]): GraphPoint => {
      const cell = part.cells.get(`${key(groupBy.length ? [x.key] : [])}\u0001${key(groupBy.length > 1 ? [s.key] : [])}`);
      return { value: cell?.values.get(measure.spec) ?? 0, domain: cell ? [...this.heldDomain, ...x.domain, ...s.domain] : undefined };
    };
    let rows = xs.map(x => ({ label: x.label, points: ss.map(s => point(x, s)) }));
    if (!rows.some(row => row.points.some(item => item.domain !== undefined))) return empty;
    if (order && mode !== "pie") { // reference graph_model.js: the axis by each group's total, ties in the server's order
      const sum = (row: (typeof rows)[number]): number => row.points.reduce((acc, item) => acc + item.value, 0);
      rows = rows.map((row, index) => ({ row, index })).sort((a, b) => (sum(a.row) - sum(b.row)) * (order === "asc" ? 1 : -1) || a.index - b.index).map(item => item.row);
    }
    const series: GraphSeries[] = ss.map((s, index) => {
      let running = 0;
      return { label: s.label, points: rows.map(row => {
        const item = row.points[index]!;
        if (!(cumulated && mode === "line")) return item;
        running += item.value;
        return { ...item, value: running };
      }) };
    });
    // A pie draws only what is above zero (reference graph_model.js).
    const slices = rows.flatMap(row => row.points.map((item, index) => ({ label: groupBy.length > 1 ? `${row.label} / ${ss[index]!.label}` : row.label, value: item.value, domain: item.domain, index: 0 })))
      .filter(slice => slice.value > 0).map((slice, index) => ({ ...slice, index }));
    return { labels: rows.map(row => row.label), series, slices, measure, empty: false, unavailable: false };
  }
}

export function archState(config: GraphConfig): GraphState {
  return { mode: config.mode, stacked: config.stacked, cumulated: config.cumulated, order: config.order, measure: config.measure, groupBy: config.groupBy };
}
function check(state: GraphState, config: GraphConfig): GraphState {
  if (!GRAPH_MODES.includes(state.mode)) refuse(`The graph cannot draw a ${String(state.mode)} chart.`);
  if (!config.measures.some(measure => measure.id === state.measure)) refuse(`The graph has no measure ${state.measure}.`);
  if (state.groupBy.length > GRAPH_MAX_GROUPINGS) refuse(`The graph groups by at most ${GRAPH_MAX_GROUPINGS} fields (the axis, then the series). Remove a Group By field.`);
  if (state.groupBy.length === 2 && groupingKey(state.groupBy[0]!) === groupingKey(state.groupBy[1]!)) refuse(`The graph is already grouped by ${state.groupBy[0]!.label}. Choose another field.`);
  return state;
}

// ---- the address ----

export const GRAPH_PARAMS = ["graph_mode", "graph_measure", "graph_groupby", "graph_order", "graph_stacked", "graph_cumulated"] as const;
export type GraphParams = Partial<Record<(typeof GRAPH_PARAMS)[number], string>>;
const specs = (fields: readonly PivotField[]): string => fields.map(specOf).join(",");

/** The address parameters of a state: only what differs from the arch, so an untouched chart keeps a clean address. */
export function graphParams(state: GraphState, config: GraphConfig): GraphParams {
  const params: GraphParams = {};
  if (state.mode !== config.mode) params.graph_mode = state.mode;
  if (state.measure !== config.measure) params.graph_measure = state.measure;
  if (specs(state.groupBy) !== specs(config.groupBy)) params.graph_groupby = specs(state.groupBy);
  if (state.order !== config.order) params.graph_order = state.order ?? "none";
  if (state.stacked !== config.stacked) params.graph_stacked = state.stacked ? "1" : "0";
  if (state.cumulated !== config.cumulated) params.graph_cumulated = state.cumulated ? "1" : "0";
  return params;
}
/** The state an address describes over the arch's. A malformed value is refused, never dropped. */
export function graphParamsState(params: GraphParams, config: GraphConfig): GraphState {
  const state = archState(config);
  const flag = (value: string | undefined, name: string, fallback: boolean): boolean => value === undefined ? fallback : value === "1" ? true : value === "0" ? false : refuse(`The page address has an invalid ${name}.`);
  if (params.graph_mode !== undefined) state.mode = (GRAPH_MODES as readonly string[]).includes(params.graph_mode) ? params.graph_mode as GraphMode : refuse("The page address has an invalid graph_mode.");
  if (params.graph_measure !== undefined) state.measure = params.graph_measure;
  if (params.graph_groupby !== undefined) state.groupBy = params.graph_groupby.split(",").map(spec => graphGrouping(spec, config.groupable));
  if (params.graph_order !== undefined) state.order = params.graph_order === "none" ? null : params.graph_order === "asc" || params.graph_order === "desc" ? params.graph_order : refuse("The page address has an invalid graph_order.");
  state.stacked = flag(params.graph_stacked, "graph_stacked", state.stacked);
  state.cumulated = flag(params.graph_cumulated, "graph_cumulated", state.cumulated);
  return check(state, config);
}
