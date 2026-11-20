/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import type { WidgetEnvironment, WidgetHandle } from "@erpforall/client";

/** Edit the provider's JSON array as one exact model identifier per line. */
export function modelList({ field, value, change }: WidgetEnvironment): WidgetHandle {
  const node = document.createElement("label");
  node.className = "field-label";
  const caption = document.createElement("span");
  caption.textContent = field.label;
  const input = document.createElement("textarea");
  input.name = field.name;
  input.rows = 4;
  input.spellcheck = false;
  input.readOnly = field.readonly;
  input.required = field.required;
  if (value !== null && value !== undefined && value !== false) {
    if (!Array.isArray(value) || !value.every(id => typeof id === "string")) {
      throw new Error("The saved model list must contain model identifiers.");
    }
    input.value = value.join("\n");
  }
  function read(): string[] {
    const ids = input.value.split("\n").filter(line => line !== "");
    let error = "";
    if (!ids.length) error = "Enter at least one model identifier.";
    else if (ids.some(id => !/^[!-~]{1,1024}$/.test(id))) error = "Use one model identifier per line, without spaces (maximum 1024 characters each).";
    else if (new Set(ids).size !== ids.length) error = "Each model identifier must appear only once.";
    input.setCustomValidity(error);
    input.setAttribute("aria-invalid", String(Boolean(error)));
    return ids;
  }
  input.oninput = () => { if (!field.readonly) read(); };
  input.onchange = () => {
    if (field.readonly) return;
    const ids = read();
    if (input.validity.valid) change(ids);
  };
  node.append(caption, input);
  return { node, dispose() { input.oninput = null; input.onchange = null; } };
}
