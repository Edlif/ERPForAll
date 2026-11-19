/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { configureClientTemplates, registerClientAction } from "../src/client/registry.js";
import type { ClientActionEnvironment } from "../src/client/registry.js";
import { HumanApp } from "../src/client/app.js";

const bootstrap = {
  wire_major: 1, registry_version: "r1", asset_version: "a1",
  identity: { uid: 2, name: "Operator", internal: true },
  companies: [{ id: 1, name: "Arkana" }], current_company_id: 1, allowed_company_ids: [1], active_company_ids: [1],
  locale: { lang: "en_US", tz: "UTC", date_format: "%m/%d/%Y", time_format: "%H:%M:%S" },
  csrf: { logout: "test-token-not-secret" }
};
const json = (value: unknown) => new Response(JSON.stringify(value), { headers: { "Content-Type": "application/json" } });
const menus = { wire_major: 1, registry_version: "r1", roots: [10, 11], nodes: [
  { module: null, id: 10, parent_id: null, label: "Board", action_id: 30, action_kind: "client", children: [], icon: null },
  { module: null, id: 11, parent_id: null, label: "Ghost", action_id: 31, action_kind: "client", children: [], icon: null }
] };
const clientAction = (id: number, tag: string) => ({ wire_major: 1, result: { kind: "client", id, name: `Action ${id}`, tag, params: { report: 7 } } });

let app: HumanApp | undefined;
beforeEach(() => configureClientTemplates([{
  name: "web.HumanControlPanel",
  render: () => {
    const root = document.createElement("section");
    root.setAttribute("data-control-panel", "");
    for (const key of ["breadcrumbs", "search", "actions", "extension", "tools"]) {
      const node = document.createElement("div");
      node.setAttribute(`data-control-${key}`, "");
      root.append(node);
    }
    return root;
  }
}]));
afterEach(() => { app?.dispose(); app = undefined; vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); history.replaceState(null, "", "/"); });
function host(): HTMLElement {
  const root = document.createElement("div");
  for (const region of ["header", "navigation", "notice", "content"]) { const node = document.createElement("div"); node.setAttribute(`data-client-${region}`, ""); root.append(node); }
  document.body.append(root);
  return root;
}
function fetchMock(tags: Record<number, string>) {
  return vi.fn(async (path: string, init?: RequestInit) => {
    if (path.endsWith("erp_load_web_menus")) return json(menus);
    if (path.endsWith("ir.actions.client/erp_load_web_client_action")) {
      const id = (JSON.parse(String(init?.body)) as { ids: number[] }).ids[0]!;
      return json(clientAction(id, tags[id]!));
    }
    return json([]);
  });
}

/// (partial: registration is unique and validated; the server-side duplicate is a composition error)
test("a client action tag registers once and only in the addon.name form", () => {
  const factory = { create: async () => ({ dispose() {} }) };
  registerClientAction("demo.duplicate", factory);
  expect(() => registerClientAction("demo.duplicate", factory)).toThrowError(expect.objectContaining({ message: "Duplicate client action: demo.duplicate." }));
  for (const bad of ["board", "Demo.board", "demo.Board", "demo.", `demo.${"a".repeat(64)}`]) {
    expect(() => registerClientAction(bad, factory)).toThrowError(expect.objectContaining({ kind: "validation" }));
  }
});

/// (partial: a menu of kind client mounts the registered screen with its params; disposal is once)
test("a client menu mounts its registered screen and disposes it with the page", async () => {
  history.replaceState(null, "", "/erp/action-30?menu=10");
  const seen: ClientActionEnvironment[] = [];
  const dispose = vi.fn();
  registerClientAction("demo.board", { create: async environment => {
    seen.push(environment);
    const node = document.createElement("p"); node.dataset["screen"] = "board"; node.textContent = `report ${String(environment.action.params["report"])}`;
    environment.host.append(node);
    return { dispose, captureState: () => ({ scroll: 3 }) };
  } });
  vi.stubGlobal("fetch", fetchMock({ 30: "demo.board" }));
  const root = host();
  app = new HumanApp(root, bootstrap);
  await app.start();
  expect(seen).toHaveLength(1);
  expect(seen[0]!.action).toEqual({ id: 30, name: "Action 30", tag: "demo.board", params: { report: 7 } });
  expect(seen[0]!.restored).toBeNull();
  expect(root.querySelector("[data-screen=board]")?.textContent).toBe("report 7");
  app.dispose(); app.dispose();
  expect(dispose).toHaveBeenCalledTimes(1);
});

/// (partial: a tag the server sent that the bundle does not register fails by name and mounts nothing)
test("an unregistered client action tag fails by name and mounts nothing", async () => {
  history.replaceState(null, "", "/erp/action-31?menu=11");
  vi.stubGlobal("fetch", fetchMock({ 31: "demo.ghost" }));
  const root = host();
  app = new HumanApp(root, bootstrap);
  await expect(app.start()).rejects.toMatchObject({
    kind: "unsupported",
    message: "Client action 'demo.ghost' is declared by the server but not registered by this page. Reload the page."
  });
  expect(root.querySelector("[data-screen]")).toBeNull();
});
