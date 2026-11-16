/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test } from "vitest";
import { describeViews } from "../src/client/metadata.js";
import "../src/client/fields.js";

// The company form shows its logo (a binary field) with the image widget; the description must be accepted.
test("a binary field with the image widget is accepted in a view description", () => {
  const wire = { notices: [], wire_major: 1, registry_version: "r1", description_version: "d1", model: "res.company", fields: { "res.company": {
    logo: { type: "binary", label: "Company Logo", readonly: false, required: false, selection: null, relation: null, relation_field: null },
  } }, views: [{ identity: { kind: "stored", id: 1 }, type: "form", renderer: "web.form", schema_major: 1, root: 0, nodes: [
    { kind: "form", record_mode: "record", children: [1] },
    { kind: "field", field: { model: "res.company", name: "logo" }, widget: "image", label: "Company Logo", force_save: false, modifiers: { readonly: null, required: null, invisible: null }, options: {} },
  ] }] };
  const view = describeViews(wire, "res.company", "r1").views.get("form")!;
  expect(view.fields[0]).toMatchObject({ name: "logo", kind: "binary", widget: "image" });
});
