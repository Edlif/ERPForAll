/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
// PDF, XLSX and Print (design REPORTING.CORE §3, §12): the server recomputes the file from the options as the caller
// (`POST /erp/reporting/export`); the screen posts the report id, the options and the format, never rows (DYNREP-037).
import { icon, saveFile } from "@erpforall/client";
import type { ClientActionEnvironment, IconName } from "@erpforall/client";
import type { Options } from "./report-data.js";

const FORMATS = { pdf: "application/pdf", xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" } as const;
type Format = keyof typeof FORMATS;

export interface ExportHost {
  readonly env: ClientActionEnvironment; readonly report: number;
  /** The options being shown, or undefined while nothing is shown yet. */
  options(): Options | undefined;
  /** Called when an export starts or ends, so the screen can recompute which buttons are enabled. */
  changed(): void;
}
export interface Exports { readonly buttons: ReadonlyMap<string, HTMLButtonElement>; busy(): boolean }

export function exportButtons(host: ExportHost): Exports {
  let exporting = false;
  const run = (format: Format, print: boolean): void => {
    const options = host.options();
    if (!options || exporting) return;
    host.env.run(async () => {
      const viewer = print ? window.open("about:blank", "_blank") : null;
      if (viewer) viewer.opener = null;
      exporting = true; host.changed();
      try {
        const { blob, fileName } = await host.env.rpc.postFile("/erp/reporting/export", { report: host.report, options, format }, FORMATS[format]);
        if (viewer && !viewer.closed) {
          const url = URL.createObjectURL(blob);
          try { viewer.location.replace(url); }
          finally { setTimeout(() => URL.revokeObjectURL(url), 60_000); }
        } else saveFile(blob, fileName);
      } catch (error) {
        viewer?.close();
        throw error;
      } finally { exporting = false; host.changed(); }
    });
  };
  const buttons = new Map<string, HTMLButtonElement>();
  const add = (key: string, text: string, iconName: IconName | null, format: Format, print: boolean): void => {
    const node = document.createElement("button"); node.type = "button"; node.className = "erp-button report-button"; node.dataset["action"] = key;
    if (iconName) node.append(icon(iconName));
    node.append(text); node.addEventListener("click", () => run(format, print)); buttons.set(key, node);
  };
  add("pdf", "PDF", null, "pdf", false); add("xlsx", "XLSX", null, "xlsx", false); add("print", "Print", "printer", "pdf", true);
  return { buttons, busy: () => exporting };
}
