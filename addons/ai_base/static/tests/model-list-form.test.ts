/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, expect, test, vi } from "vitest";
import "../../../web/static/src/client/fields.js";
import "../src/client.js";
import { RecordForm } from "../../../web/static/src/client/record-form.js";
import { RecordState } from "../../../web/static/src/client/record.js";
import { describeViews } from "../../../web/static/src/client/metadata.js";
import { RpcClient } from "../../../web/static/src/client/transport.js";

const dispose: (() => void)[] = [];
afterEach(() => { dispose.splice(0).forEach(stop => stop()); document.body.replaceChildren(); });
async function setup() {
  const writes: unknown[] = [];
  // Mirrors core.rs web_read/onchange/web_save request keys; real shapes also run in served_ai_config.
  const rpc = new RpcClient(async (url, init) => {
    const body = JSON.parse(String(init?.body));
    expect(body.ids).toEqual([1]);
    const method = String(url).split("/").at(-1);
    if (method === "web_read") {
      expect(body.specification).toHaveProperty("allowed_models");
      return new Response(JSON.stringify([{id: 1, name: "local", allowed_models: ["original"]}]));
    }
    if (method === "onchange") {
      expect(body.fields_spec).toHaveProperty("allowed_models");
      expect(body.field_names).toEqual(["allowed_models"]);
      return new Response(JSON.stringify({value: {}}));
    }
    if (method === "web_save") {
      expect(body.specification).toHaveProperty("allowed_models");
      writes.push(body.vals);
      return new Response(JSON.stringify([{id: 1, name: "local", allowed_models: ["original"], ...body.vals}]));
    }
    throw new Error(`Unexpected RPC ${method}`);
  }, () => {});
  const field = (type: string, label: string) => ({type, label, required: true, readonly: false, relation: null, relation_field: null, selection: null});
  const node = (name: string, label: string, widget: string) => ({kind: "field", field: {model: "ai.provider", name}, widget, label, force_save: false, options: {}, modifiers: {readonly: null, required: null, invisible: null}});
  const description = describeViews({wire_major: 1, registry_version: "r1", description_version: "d1", model: "ai.provider", notices: [],
    fields: {"ai.provider": {name: field("char", "Name"), allowed_models: field("text", "Allowed models")}},
    views: [{identity: {kind: "stored", id: 1}, type: "form", schema_major: 1, renderer: "web.form", root: 0, nodes: [
      {kind: "form", record_mode: "record", children: [1, 2]}, node("name", "Name", "char"), node("allowed_models", "Allowed models", "ai_base.model_list")]}]}, "ai.provider", "r1");
  const view = description.views.get("form")!;
  const state = new RecordState(rpc, "ai.provider", view.fields, {});
  await state.load(1);
  const form = document.createElement("form"); document.body.append(form);
  const surface = new RecordForm({form, state, view, rpc, context: {}, run: () => {}, report: error => { throw error; }, openRecord: async () => {}, button: () => {},
    stage: (name, value) => state.change(name, value),
    commit: async name => { await state.onchange(name); surface.render(); },
    change: (name, value) => { state.values = {...state.values, [name]: value}; surface.render(); }});
  surface.render(); dispose.push(() => surface.dispose());
  const input = () => form.querySelector<HTMLTextAreaElement>("textarea")!;
  const type = (value: string) => { input().value = value; input().dispatchEvent(new Event("input", {bubbles: true})); };
  return {form, state, surface, input, type, writes};
}
test("invalid model text survives an unrelated redraw and blocks saving the previous array", async () => {
  const {state, surface, input, type} = await setup();
  type("two words");
  expect(state.dirty).toBe(true);
  state.values = {...state.values, name: "renamed"}; surface.render();
  expect(input().value).toBe("two words");
  expect(surface.valid()).toBe(false);
  expect(document.activeElement).toBe(input());
  const save = vi.fn(); if (surface.valid(false)) save(state.values);
  expect(save).not.toHaveBeenCalled();
  expect(state.values["allowed_models"]).toEqual(["two words"]);
  type("provider/valid"); input().dispatchEvent(new Event("change", {bubbles: true}));
  expect(surface.valid()).toBe(true);
  expect(state.values["allowed_models"]).toEqual(["provider/valid"]);
});
test("a valid local draft survives an unrelated redraw before change fires", async () => {
  const {state, surface, input, type} = await setup();
  type("provider/new");
  expect(state.dirty).toBe(true);
  state.values = {...state.values, name: "renamed"}; surface.render();
  expect(input().value).toBe("provider/new");
  input().dispatchEvent(new Event("change", {bubbles: true}));
  expect(state.values["allowed_models"]).toEqual(["provider/new"]);
});
test("discard drops local invalid text instead of restoring it", async () => {
  const {state, surface, input, type} = await setup();
  type("two words"); state.discard(); surface.render();
  expect(input().value).toBe("original");
  expect(surface.valid()).toBe(true);
});

test("Save flushes a valid unblurred draft and blocks an invalid staged array", async () => {
  const {state, surface, type, writes} = await setup();
  type("two words");
  await expect(state.save()).rejects.toMatchObject({kind: "validation", message: "Complete the required fields before saving."});
  expect(writes).toEqual([]);
  type("provider/new");
  await state.save();
  expect(writes).toEqual([{allowed_models: ["provider/new"]}]);
  expect(state.dirty).toBe(false);
  expect(surface.valid()).toBe(true);
});
