/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test } from "vitest";
import { clientActions } from "../../../web/static/src/client/registry.js";
import "../src/client.js";
import { linesAnswer, optionsAnswer, REPORT, rig, settle } from "./support.js";

/// (partial: the addon's entry module registers its tag once, through the public registry, and the page can mount it)
test("the entry module registers reporting.report and the registered screen mounts and disposes", async () => {
  expect(clientActions.has("reporting.report")).toBe(true);
  const r = rig({ erp_report_options: () => optionsAnswer(), erp_report_lines: () => linesAnswer([]) }, { report: REPORT });
  const handle = await clientActions.get("reporting.report").create(r.env);
  await settle();
  expect(r.env.host.querySelector(".report-screen")).not.toBeNull();
  expect(handle.captureState?.()).toMatchObject({ options: { search: "" } });
  handle.dispose();
  expect(r.env.host.querySelector(".report-screen")).toBeNull();
  r.dispose();
});
