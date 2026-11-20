/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import type { WidgetEnvironment, WidgetHandle } from "@erpforall/client";

const scope = "primary_unbooked_receipt_projection";
type Row = Record<string, unknown>;
function row(value: unknown): Row {
  if (value === null || typeof value !== "object" || Array.isArray(value)) throw new Error("Invalid valuation response.");
  return value as Row;
}
function id(value: unknown): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value <= 0) throw new Error("Invalid record in the valuation response.");
  return value;
}
function exact(value: unknown): string {
  const { numerator, denominator } = row(value);
  if (typeof numerator !== "string" || !/^-?\d+$/.test(numerator) || typeof denominator !== "string" || !/^[1-9]\d*$/.test(denominator)) throw new Error("Invalid exact valuation amount.");
  return denominator === "1" ? numerator : numerator + "/" + denominator;
}
function button(label: string): HTMLButtonElement {
  const el = document.createElement("button");
  el.type = "button"; el.className = "erp-button"; el.textContent = label;
  return el;
}
function labelled(label: string, control: HTMLElement): HTMLLabelElement {
  const el = document.createElement("label");
  el.append(label, control); return el;
}
function summary(label: string, value: string): HTMLDivElement {
  const el = document.createElement("div"), name = document.createElement("dt"), data = document.createElement("dd");
  name.textContent = label; data.textContent = value; el.append(name, data); return el;
}
export function issueCostPreview(env: WidgetEnvironment): WidgetHandle {
  const node = document.createElement("section"), controls = document.createElement("div");
  node.className = "stock-issue-preview"; controls.className = "stock-issue-controls";
  const quantity = document.createElement("input"), method = document.createElement("select");
  quantity.type = "text"; quantity.inputMode = "decimal"; quantity.autocomplete = "off";
  for (const code of ["fifo", "lifo"]) method.add(new Option(code.toUpperCase(), code));
  const submit = button("Preview cost"); submit.disabled = true;
  controls.append(labelled("Issue quantity", quantity), labelled("Cost method", method), submit);
  const explanation = document.createElement("p");
  explanation.textContent = "Estimate from company-owned receipt layers for this product and lot across locations. Previewing leaves stock and journal entries unchanged.";
  const status = document.createElement("div"), result = document.createElement("div");
  status.setAttribute("role", "status"); node.append(controls, explanation, status, result);
  let active: AbortController | undefined;
  const reset = (): void => {
    active?.abort(); active = undefined; result.replaceChildren(); status.textContent = "";
    status.setAttribute("role", "status"); submit.disabled = !quantity.value.trim();
  };
  function show(data: Row, currency: string): void {
    const receipts = data["allocations"];
    if (!Array.isArray(receipts) || !receipts.length) throw new Error("The preview returned no receipt allocation.");
    const details = document.createElement("dl"), allocation = document.createElement("div");
    details.append(summary("Issue cost", exact(data["total"]) + " " + currency),
      summary("Pool quantity remaining", exact(data["pool_remaining_quantity"])),
      summary("Pool value remaining", exact(data["pool_remaining_value"]) + " " + currency));
    const display = (index: number): void => {
      const item = row(receipts[index]), origin = row(item["origin"]);
      const recordId = id(origin["id"]), model = origin["model"], effective = item["effective_time"];
      if (typeof model !== "string" || typeof effective !== "string") throw new Error("Invalid receipt source in the valuation response.");
      const caption = document.createElement("p"), open = button("Open source");
      caption.textContent = `Receipt ${index + 1} of ${receipts.length} · ${effective} · ${exact(item["quantity"])} units · ${exact(item["value"])} ${currency}`;
      open.disabled = !env.openRecord;
      open.onclick = () => { if (env.openRecord) env.run(() => env.openRecord!(model, recordId)); };
      allocation.replaceChildren(caption, open);
      if (receipts.length > 1) {
        const prev = button("Previous receipt"), next = button("Next receipt");
        prev.disabled = index === 0; next.disabled = index + 1 === receipts.length;
        prev.onclick = () => navigate(index - 1); next.onclick = () => navigate(index + 1);
        allocation.append(prev, next);
      }
    };
    function navigate(index: number): void {
      try { display(index); } catch (error) { failed(error); }
    }
    display(0); result.replaceChildren(details, allocation);
  }
  function failed(error: unknown): void {
    result.replaceChildren(); status.setAttribute("role", "alert");
    status.textContent = error instanceof Error ? error.message : String(error);
  }
  async function preview(): Promise<void> {
    reset();
    if (submit.disabled || env.signal?.aborted) return;
    const request = new AbortController(); active = request; submit.disabled = true;
    status.textContent = "Calculating issue cost…";
    try {
      const costMethod = method.value;
      const data = row(await env.rpc.call("stock.quant", "preview_issue_cost", { ids: [id(env.value)], context: env.context,
        request: { scope, method: costMethod, quantity: quantity.value.trim() } }, { signal: request.signal }));
      if (request.signal.aborted) return;
      if (data["scope"] !== scope || data["comparison_only"] !== true || data["method"] !== costMethod) throw new Error("The valuation response does not match this preview.");
      const currencyId = id(data["currency_id"]);
      const rows = await env.rpc.call("res.currency", "web_read", { ids: [currencyId], specification: { name: {} }, context: env.context }, { signal: request.signal });
      if (request.signal.aborted) return;
      if (!Array.isArray(rows) || rows.length !== 1) throw new Error("The preview currency is unavailable.");
      const currency = row(rows[0]);
      if (currency["id"] !== currencyId || typeof currency["name"] !== "string" || !currency["name"]) throw new Error("The preview currency is unavailable.");
      show(data, currency["name"]); status.textContent = "Preview ready.";
    } catch (error) {
      if (!request.signal.aborted) failed(error);
    } finally {
      if (active === request) { active = undefined; submit.disabled = !quantity.value.trim(); }
    }
  }
  submit.onclick = () => void preview(); quantity.oninput = reset; method.onchange = reset;
  quantity.onkeydown = event => { if (event.key === "Enter") { event.preventDefault(); event.stopPropagation(); if (!submit.disabled) void preview(); } };
  env.signal?.addEventListener("abort", reset, {once: true});
  return { node, dispose() { env.signal?.removeEventListener("abort", reset); reset(); } };
}
