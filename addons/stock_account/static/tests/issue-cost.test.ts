/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test, vi } from "vitest";
import type { WidgetEnvironment } from "@erpforall/client";
import { issueCostPreview } from "../src/issue-cost.js";

const fraction = (n: string, d = "1") => ({ numerator: n, denominator: d });
const response = {
  scope: "primary_unbooked_receipt_projection", comparison_only: true, method: "lifo",
  currency_id: 7, total: fraction("9007199254740993", "3"), pool_remaining_value: fraction("66"),
  pool_remaining_quantity: fraction("6"), allocations: [{
    effective_time: "2026-01-01", origin: { model: "stock.move", id: 12 },
    quantity: fraction("6"), value: fraction("9007199254740993", "3")
  }]
};
function fixture(call: ReturnType<typeof vi.fn>) {
  const openRecord = vi.fn();
  const env = { value: 42, context: { allowed_company_ids: [2] }, rpc: { call },
    openRecord, run: (job: () => Promise<void>) => { void job(); } } as unknown as WidgetEnvironment;
  const widget = issueCostPreview(env);
  const qty = widget.node.querySelector("input")!;
  qty.value = "6"; qty.dispatchEvent(new Event("input"));
  const method = widget.node.querySelector("select")!;
  method.value = "lifo"; method.dispatchEvent(new Event("change"));
  return { ...widget, qty, openRecord, preview: () => widget.node.querySelector("button")!.click() };
}
test("uses the chosen method and exact quantity, then opens the readable source", async () => {
  const call = vi.fn().mockResolvedValueOnce(response).mockResolvedValueOnce([{id: 7, name: "IDR"}]);
  const w = fixture(call);
  w.preview();
  await vi.waitFor(() => expect(w.node.textContent).toContain("9007199254740993/3 IDR"));
  expect(call.mock.calls[0]?.slice(0, 3)).toEqual(["stock.quant", "preview_issue_cost", {
    ids: [42], request: { scope: response.scope, method: "lifo", quantity: "6" }, context: {allowed_company_ids: [2]}
  }]);
  [...w.node.querySelectorAll("button")].find(b => b.textContent === "Open source")!.click();
  expect(w.openRecord).toHaveBeenCalledWith("stock.move", 12);
  w.dispose();
});
test("changing quantity drops a pending result and access denials clear the prior values", async () => {
  let finish!: (value: unknown) => void;
  const call = vi.fn().mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }))
    .mockRejectedValueOnce(new Error("Source access denied"));
  const w = fixture(call);
  w.preview(); w.qty.value = "7"; w.qty.dispatchEvent(new Event("input")); finish(response);
  await Promise.resolve();
  expect(w.node.textContent).not.toContain("9007199254740993");
  w.preview();
  await vi.waitFor(() => expect(w.node.querySelector('[role="alert"]')?.textContent).toContain("Source access denied"));
  expect(call).toHaveBeenCalledTimes(2);
  w.dispose();
});
