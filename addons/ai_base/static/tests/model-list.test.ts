/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test, vi } from "vitest";
import type { WidgetEnvironment } from "@erpforall/client";
import { modelList } from "../src/model-list.js";

function setup(value: unknown, readonly = false) {
  const change = vi.fn();
  const widget = modelList({value, change, field: {name: "allowed_models", label: "Allowed models", required: true, readonly}} as unknown as WidgetEnvironment);
  const input = widget.node.querySelector("textarea")!;
  const edit = (text: string) => {
    input.value = text;
    input.dispatchEvent(new Event("input"));
    input.dispatchEvent(new Event("change"));
  };
  return { input, change, edit };
}
test("opens persisted model identifiers and saves the edited JSON array", () => {
  const {input, edit, change} = setup(["one", "two"]);
  expect(input.value).toBe("one\ntwo");
  edit("two\nprovider/new-model\n");
  expect(change).toHaveBeenCalledWith(["two", "provider/new-model"]);
});
test("rejects an empty list, duplicate names and embedded whitespace", () => {
  const {input, edit, change} = setup(false);
  for (const wrong of ["", "first\nfirst", "two words"]) {
    edit(wrong);
    expect(input.checkValidity()).toBe(false);
  }
  expect(change).not.toHaveBeenCalled();
  edit("provider/model");
  expect(input.checkValidity()).toBe(true);
  expect(change).toHaveBeenCalledOnce();
});
test("preserves readonly mode and rejects malformed stored values", () => {
  const {input, edit, change} = setup(["one"], true);
  expect(input.readOnly).toBe(true);
  edit("two");
  expect(change).not.toHaveBeenCalled();
  expect(() => setup({model: "one"})).toThrow();
});
