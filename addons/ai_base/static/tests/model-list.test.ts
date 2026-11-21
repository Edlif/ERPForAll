/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test, vi } from "vitest";
import type { WidgetEnvironment } from "@erpforall/client";
import { modelList } from "../src/model-list.js";

function setup(value: unknown, readonly = false) {
  const change = vi.fn(), stage = vi.fn();
  const widget = modelList({value, change, draft: {stage, commit: () => change(stage.mock.calls.at(-1)?.[0])}, field: {name: "allowed_models", label: "Allowed models", required: true, readonly}} as unknown as WidgetEnvironment);
  const input = widget.node.querySelector("textarea")!;
  const edit = (text: string) => {
    input.value = text;
    input.dispatchEvent(new Event("input"));
    input.dispatchEvent(new Event("change"));
  };
  return { input, change, stage, edit, widget };
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
  expect(() => setup({model: "one"})).toThrowError(new Error("The saved model list must contain model identifiers."));
});

test("identifier boundaries and newline normalization match the server contract", () => {
  const {input, edit, change, widget} = setup(["one"]);
  const message = "Use one model identifier per line, without spaces (maximum 1024 characters each).";
  for (const value of ["x".repeat(1025), "é", "with\tspace"]) {
    edit(value); expect(input.validationMessage).toBe(message);
  }
  expect(change).not.toHaveBeenCalled();
  edit("x".repeat(1024)); expect(input.checkValidity()).toBe(true);
  edit("one\r\ntwo\r\n"); expect(change).toHaveBeenLastCalledWith(["one", "two"]);
  widget.dispose(); edit("after-dispose");
  expect(change).toHaveBeenLastCalledWith(["one", "two"]);
});

test("authoritative replacements clear prior errors without validating untouched forms", () => {
  const {input, edit, widget} = setup(false);
  const field = {name: "allowed_models", label: "Allowed models", required: true, readonly: false};
  widget.draft!.update([], field);
  expect(input.getAttribute("aria-invalid")).not.toBe("true");
  edit("two words");
  expect(input.validationMessage).not.toBe("");
  widget.draft!.update(["valid"], field);
  expect(input.validationMessage).toBe("");
  expect(input.getAttribute("aria-invalid")).toBe("false");
  edit("two words");
  widget.draft!.update(["two words"], {...field, readonly: true});
  expect(input.validationMessage).toBe("");
});
test("disabled editors refuse synthetic events and errors describe their control", () => {
  const {input, edit, widget, stage, change} = setup(["one"]);
  widget.draft!.setDisabled(true);
  edit("two");
  expect(stage).not.toHaveBeenCalled();
  expect(change).not.toHaveBeenCalled();
  widget.draft!.setDisabled(false);
  edit("two words");
  const note = widget.node.querySelector(".field-error")!;
  expect(note.id).not.toBe("");
  expect(input.getAttribute("aria-describedby")).toBe(note.id);
  expect(note.textContent).toBe(input.validationMessage);
});
