/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import type { ClientActionEnvironment } from "@erpforall/client";

const sourceFields = {
  "sale.order": { label: "Sales orders", date: "date_order" },
  "purchase.order": { label: "Purchase orders", date: "date_order" },
  "stock.picking": { label: "Inventory transfers", date: "scheduled_date" },
  "account.move": { label: "Journal entries", date: "date" },
} as const;
type Source = keyof typeof sourceFields;

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid workspace data.");
  return value as Record<string, unknown>;
}
function isSource(value: unknown): value is Source {
  return typeof value === "string" && Object.prototype.hasOwnProperty.call(sourceFields, value);
}
function node<K extends keyof HTMLElementTagNameMap>(name: K, text?: string): HTMLElementTagNameMap[K] {
  const element = document.createElement(name);
  if (text !== undefined) element.textContent = text;
  return element;
}
function selections(raw: unknown): Map<string, string> {
  const pairs = object(object(raw)["state"])["selection"];
  if (!Array.isArray(pairs)) throw new Error("Status descriptions could not be read.");
  const labels = new Map<string, string>();
  for (const pair of pairs) {
    if (!Array.isArray(pair) || typeof pair[0] !== "string" || typeof pair[1] !== "string") {
      throw new Error("Invalid status description.");
    }
    labels.set(pair[0], pair[1]);
  }
  return labels;
}

function openRecord(env: ClientActionEnvironment, model: Source, id: number, name: string) {
  return env.open({ type: "ir.actions.act_window", name, res_model: model, res_id: id, views: [[false, "form"]], target: "new" });
}

/** Resolve a proposed business reference only on an explicit click under this action's read scope. */
export function recordProposal(env: ClientActionEnvironment, company: number) {
  const sources = env.action.params["sources"];
  return (model: string, reference: string) => {
    if (!isSource(model) || !Array.isArray(sources) || !sources.includes(model)) return;
    return async () => {
      const { length, records } = object(await env.rpc.call(model, "web_search_read", {
        specification: { name: {} }, context: { allowed_company_ids: [company] },
        domain: [["company_id", "=", company], ["name", "=", reference]], offset: 0, limit: 2, count_limit: 2,
      }));
      if (!Array.isArray(records) || typeof length !== "number" || !Number.isSafeInteger(length)
          || length < 0 || records.length !== Math.min(length, 2)) throw new Error("Invalid record search response.");
      if (!length) throw new Error("No matching reference is visible in the current company.");
      if (length > 1) throw new Error("Several records share this reference. Choose one under Find ERP records.");
      const row = object(records[0]);
      if (row["name"] !== reference || typeof row["id"] !== "number" || !Number.isSafeInteger(row["id"]) || row["id"] < 1)
        throw new Error("Invalid record search response.");
      await openRecord(env, model, row["id"], reference);
    };
  };
}

/** An explicit read on the human RPC surface, separate from the provider's input. */
export function workspaceRecords(env: ClientActionEnvironment, company: number) {
  const { sources, limit } = env.action.params;
  if (!Array.isArray(sources) || !sources.every(isSource) || typeof limit !== "number" || !Number.isSafeInteger(limit) || limit < 1) {
    throw new Error("Invalid workspace search configuration.");
  }
  const section = node("section");
  section.className = "ai-workspace-records";
  const head = node("h2", "Find ERP records");
  section.append(head);
  const companyName = env.companies.find(item => item.id === company)?.name;
  if (companyName) {
    const scope = node("p", companyName);
    scope.className = "ai-workspace-company";
    section.append(scope);
  }
  let generation = 0;
  if (!sources.length) {
    section.append(node("p", "No retail records are available for your account."));
    return { node: section, dispose() {} };
  }
  const form = node("form");
  const picker = node("select");
  picker.setAttribute("aria-label", "Record type");
  for (const source of sources) picker.add(new Option(sourceFields[source].label, source));
  const search = node("input");
  search.type = "search"; search.placeholder = "Search by reference";
  search.setAttribute("aria-label", "Record reference");
  const submit = node("button", "Search");
  submit.type = "submit"; submit.className = "erp-button";
  const status = node("p", "Search by reference or leave it blank to see recent records.");
  status.setAttribute("role", "status");
  const results = node("div");
  results.className = "ai-workspace-results";
  form.append(picker, search, submit); section.append(form, status, results);
  const invalidate = () => {
    ++generation; status.textContent = "Search to show records for this selection.";
    results.replaceChildren(); submit.disabled = false;
  };
  picker.onchange = invalidate;
  search.oninput = invalidate;
  form.onsubmit = async event => {
    event.preventDefault();
    const source = picker.value;
    if (!isSource(source) || !sources.includes(source)) return;
    const active = ++generation;
    const text = search.value.trim();
    const context = { allowed_company_ids: [company] };
    const date = sourceFields[source].date;
    const domain: unknown[][] = [["company_id", "=", company]];
    if (text) domain.push(["name", "ilike", text]);
    submit.disabled = true; status.textContent = "Searching…"; results.replaceChildren();
    try {
      const [response, metadata] = await Promise.all([
        env.rpc.call(source, "web_search_read", {
          domain, specification: { name: {}, state: {}, [date]: {} }, context,
          offset: 0, limit, count_limit: limit + 1, order: "id desc",
        }),
        env.rpc.call(source, "fields_get", { allfields: ["state"], attributes: ["selection"], context }),
      ]);
      if (active !== generation) return;
      const labels = selections(metadata);
      const { records, length } = object(response);
      if (!Array.isArray(records) || records.length > limit || typeof length !== "number" || !Number.isSafeInteger(length) || length < records.length) {
        throw new Error("Invalid record search response.");
      }
      const table = node("table");
      table.setAttribute("aria-label", sourceFields[source].label);
      const tr = table.createTHead().insertRow();
      for (const label of ["Reference", "Status", "Date"]) {
        const th = node("th", label); th.scope = "col"; tr.append(th);
      }
      const body = table.createTBody();
      const counts = new Map<string, number>();
      for (const record of records) {
        const row = object(record);
        const { id, name, state } = row;
        if (typeof id !== "number" || !Number.isSafeInteger(id) || id < 1 ||
            typeof name !== "string" || typeof state !== "string") throw new Error("Invalid source record.");
        const dateValue = row[date];
        if (typeof dateValue !== "string" && dateValue !== false && dateValue !== null) throw new Error("Invalid record date.");
        const stateLabel = labels.get(state) ?? state;
        counts.set(stateLabel, (counts.get(stateLabel) ?? 0) + 1);
        const link = node("button", name);
        link.type = "button"; link.className = "ai-workspace-reference";
        link.onclick = () => env.run(() => openRecord(env, source, id, name));
        const line = body.insertRow();
        line.insertCell().append(link); line.insertCell().textContent = stateLabel;
        line.insertCell().textContent = typeof dateValue === "string" ? dateValue.slice(0, 10) : "—";
      }
      if (!records.length) { status.textContent = "No matching records."; return; }
      status.textContent = length > records.length ? `Showing the latest ${records.length} matching records.` :
        `${records.length} matching record${records.length === 1 ? "" : "s"}.`;
      const breakdown = node("ul");
      breakdown.setAttribute("aria-label", "Status counts for the displayed records");
      for (const [label, count] of counts) {
        const item = node("li"); item.append(node("span", label), node("strong", String(count)));
        breakdown.append(item);
      }
      results.append(breakdown, table);
    } catch (error) {
      if (active === generation) status.textContent = error instanceof Error ? error.message : "Unable to search records.";
    } finally {
      if (active === generation) submit.disabled = false;
    }
  };
  return { node: section, dispose() { ++generation; } };
}
