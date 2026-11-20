/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { button, el } from "./dom.js";
import { icon } from "./icons.js";
import type { Field } from "./metadata.js";
import type { PopoverHandle, PopoverService } from "./popover.js";

/** The first declared visible field identifies the row. Preferences affect presentation only. */
export function columnChoices(fields: readonly Field[], scope: string | undefined, popovers: PopoverService | undefined,
  changed: () => void, report: (error: unknown) => void) {
  const allowed = fields.filter(field => !field.invisible);
  const optional = allowed.slice(1).filter(field => field.name !== allowed[0]?.name);
  const key = scope && `erpforall.columns.${scope}`;
  let hidden = new Set<string>();
  if (key) {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem(key) ?? "[]");
      if (!Array.isArray(saved) || !saved.every(name => typeof name === "string")) throw new Error("Invalid saved column choices.");
      hidden = new Set(optional.map(f => f.name).filter(name => saved.includes(name)));
    } catch (error) { report(error); }
  }
  let panel: PopoverHandle | undefined;
  const trigger = button("", () => {
    if (panel) { panel.close(); return; }
    if (!popovers) return;
    const choices = el("div", undefined, "column-choices");
    for (const field of optional) {
      const label = el("label"), input = el("input");
      input.type = "checkbox"; input.checked = !hidden.has(field.name);
      input.addEventListener("change", () => {
        if (input.checked) hidden.delete(field.name); else hidden.add(field.name);
        if (key) try {
          if (hidden.size) localStorage.setItem(key, JSON.stringify([...hidden])); else localStorage.removeItem(key);
        } catch (error) { report(error); }
        changed();
      });
      label.append(input, document.createTextNode(field.label)); choices.append(label);
    }
    panel = popovers.open(trigger, choices, { role: "dialog", restoreFocus: true,
      onClose: () => { panel = undefined; trigger.setAttribute("aria-expanded", "false"); } });
    trigger.setAttribute("aria-expanded", "true");
    choices.querySelector("input")?.focus();
  }, "icon-button column-choices-trigger");
  trigger.append(icon("sliders-horizontal"));
  trigger.title = "Choose columns"; trigger.setAttribute("aria-label", trigger.title);
  trigger.setAttribute("aria-haspopup", "dialog"); trigger.setAttribute("aria-expanded", "false");
  return {
    visible: () => allowed.filter(field => !hidden.has(field.name)),
    attach(cell?: HTMLTableCellElement) {
      if (!cell || !optional.length || !popovers) return;
      cell.classList.add("column-choices-anchor"); cell.append(trigger);
    },
    dispose: () => panel?.close("dispose"),
  };
}
