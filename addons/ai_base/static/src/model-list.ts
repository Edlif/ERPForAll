/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import type { WidgetEnvironment, WidgetHandle } from "@erpforall/client";

let nextEditorId = 0;

function identifiers(value: unknown): string[] {
  if (value === null || value === undefined || value === false) return [];
  if (!Array.isArray(value) || !value.every(id => typeof id === "string")) {
    throw new Error("The saved model list must contain model identifiers.");
  }
  return value;
}

/** Stage exact model identifiers through the form's existing Save/Discard lifecycle. */
export function modelList(env: WidgetEnvironment): WidgetHandle {
  if (!env.draft) throw new Error("The model list requires the record form draft lifecycle.");
  const draft = env.draft;
  let field = env.field, disabled = false, dead = false, validated = false;
  const editorId = `ai-model-list-${++nextEditorId}`;
  const node = document.createElement("label");
  node.className = "field-label";
  const caption = document.createElement("span");
  caption.textContent = field.label;
  caption.id = `${editorId}-label`;
  const input = document.createElement("textarea");
  input.name = field.name;
  input.rows = 4;
  input.spellcheck = false;
  input.readOnly = field.readonly;
  input.required = field.required;
  input.value = identifiers(env.value).join("\n");
  const note = document.createElement("span");
  note.className = "field-error";
  note.id = `${editorId}-error`;
  input.setAttribute("aria-labelledby", caption.id);
  input.setAttribute("aria-describedby", note.id);
  note.hidden = true;
  function read(): string[] { return input.value.split("\n").filter(line => line !== ""); }
  function validate(): boolean {
    validated = true;
    const ids = read();
    let error = "";
    if (!field.readonly) {
      if (!ids.length) error = "Enter at least one model identifier.";
      else if (ids.some(id => !/^[!-~]{1,1024}$/.test(id))) error = "Use one model identifier per line, without spaces (maximum 1024 characters each).";
      else if (new Set(ids).size !== ids.length) error = "Each model identifier must appear only once.";
    }
    input.setCustomValidity(error);
    input.setAttribute("aria-invalid", String(Boolean(error)));
    node.classList.toggle("field-invalid", Boolean(error));
    note.textContent = error; note.hidden = !error;
    return !error;
  }
  input.oninput = () => {
    if (dead || field.readonly || disabled) return;
    validate();
    // Invalid arrays remain local drafts: they mark the record dirty, but validation blocks Save.
    draft.stage(read());
  };
  input.onchange = () => { if (!dead && !field.readonly && !disabled && validate()) draft.commit(); };
  node.append(caption, input, note);
  return { node, dispose() { dead = true; input.oninput = null; input.onchange = null; }, draft: {
    admit: value => { identifiers(value); },
    flush() {},
    update(value, next) {
      const ids = identifiers(value);
      if (JSON.stringify(ids) !== JSON.stringify(read())) input.value = ids.join("\n");
      field = next; input.readOnly = field.readonly; input.required = field.required;
      caption.textContent = field.label;
      if (validated) validate();
    },
    validate,
    focus: () => input.focus(),
    setDisabled(value) { disabled = value; input.disabled = value; },
  } };
}
