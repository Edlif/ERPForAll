/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, describe, expect, test, vi } from "vitest";
import { saveFile } from "../src/client/file-save.js";
import { binaryContent, forgetBinaryContent, isBinarySize } from "../src/client/binary-content.js";
import { createFieldControl } from "../src/client/fields.js";
import type { Field, Subview } from "../src/client/metadata.js";
import { RecordState } from "../src/client/record.js";
import { RpcClient } from "../src/client/transport.js";

vi.mock("../src/client/file-save.js", () => ({ saveFile: vi.fn() }));

const PNG = "iVBORw0KGgo=";
const SMALL = "iVBORw0KGgpz";
type Reader = (model: string, method: string, args: Record<string, unknown>, options?: Record<string, unknown>) => Promise<unknown>;
function client(read: Reader) {
  const call = vi.fn(read);
  return { rpc: { call } as unknown as RpcClient, call };
}
const tick = async () => { for (let i = 0; i < 6; i++) await Promise.resolve(); };

function imageField(options: Record<string, unknown> = {}, extra: Partial<Field> = {}): Field {
  return { name: "image_1920", label: "Image", kind: "binary", widget: "image", options, readonly: false, required: false, relation: null, relationField: null, selection: null, ...extra };
}
function mount(rpc: RpcClient, field: Field, values: Record<string, unknown>, extra: { model?: string; viewContext?: "form" | "list" | "card" } = { model: "res.partner" }) {
  const handle = createFieldControl({ field, value: values[field.name], values, rpc, context: { lang: "en_US" }, change: () => {}, run: () => {}, ...extra });
  document.body.append(handle.node);
  return handle;
}
const shown = (node: HTMLElement) => node.querySelector<HTMLImageElement>(".field-image-stage img")?.src;

afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });

describe("content of binary fields read with bin_size", () => {
  test("the size text is recognised, content is not", () => {
    for (const value of ["532.00 bytes", "4.88 Kb", "1.50 Mb", "2.00 Gb", "1.00 Tb"]) expect(isBinarySize(value)).toBe(true);
    for (const value of [PNG, "", "4.88 KB", "4.8 Kb", false, null, 12]) expect(isBinarySize(value)).toBe(false);
  });

  test("requests of one tick for a model and field travel as one read without bin_size", async () => {
    const { rpc, call } = client(async (_model, _method, args) => (args["ids"] as number[]).map(id => ({ id, image_128: id === 2 ? false : `${PNG.slice(0, -4)}${id}A==` })));
    const first = binaryContent(rpc, "res.partner", 1, "image_128", { lang: "en_US", bin_size: true }, "1.00 Kb");
    const second = binaryContent(rpc, "res.partner", 2, "image_128", { lang: "en_US", bin_size: true }, "2.00 Kb");
    const again = binaryContent(rpc, "res.partner", 1, "image_128", { lang: "en_US", bin_size: true }, "1.00 Kb");
    expect(again).toBe(first);
    expect(await first).toBe(`${PNG.slice(0, -4)}1A==`);
    // the file was removed after its size was read: no content, not an error
    expect(await second).toBe("");
    expect(call).toHaveBeenCalledTimes(1);
    expect(call).toHaveBeenCalledWith("res.partner", "read", { ids: [1, 2], fields: ["image_128"], context: { lang: "en_US", bin_size: false } }, { background: true, signal: expect.any(AbortSignal) });
    // kept until a view reads its records again
    await binaryContent(rpc, "res.partner", 1, "image_128", { lang: "en_US", bin_size: true }, "1.00 Kb");
    expect(call).toHaveBeenCalledTimes(1);
    // another size is another file
    await binaryContent(rpc, "res.partner", 1, "image_128", { lang: "en_US", bin_size: true }, "3.00 Kb");
    expect(call).toHaveBeenCalledTimes(2);
    forgetBinaryContent(rpc);
    await binaryContent(rpc, "res.partner", 1, "image_128", { lang: "en_US", bin_size: true }, "1.00 Kb");
    expect(call).toHaveBeenCalledTimes(3);
  });

  test("another field or model is another read", async () => {
    const { rpc, call } = client(async (_model, _method, args) => (args["ids"] as number[]).map(id => ({ id, [(args["fields"] as string[])[0]!]: PNG })));
    await Promise.all([
      binaryContent(rpc, "res.partner", 1, "image_128", {}, "1.00 Kb"),
      binaryContent(rpc, "res.partner", 1, "image_1920", {}, "9.00 Kb"),
      binaryContent(rpc, "res.company", 1, "image_128", {}, "1.00 Kb"),
    ]);
    expect(call.mock.calls.map(([model, , args]) => `${model}.${(args["fields"] as string[])[0]}`)).toEqual(["res.partner.image_128", "res.partner.image_1920", "res.company.image_128"]);
  });

  test("a record that is gone, a size in place of content and a failed read are errors that name the field", async () => {
    const gone = client(async () => []);
    await expect(binaryContent(gone.rpc, "res.partner", 9, "image_128", {}, "1.00 Kb")).rejects.toThrow("res.partner record 9 is no longer available; image_128 was not read.");
    const sized = client(async () => [{ id: 9, image_128: "1.00 Kb" }]);
    await expect(binaryContent(sized.rpc, "res.partner", 9, "image_128", {}, "1.00 Kb")).rejects.toThrow("The read of res.partner.image_128 answered no content for record 9.");
    const shape = client(async () => ({ records: [] }));
    await expect(binaryContent(shape.rpc, "res.partner", 9, "image_128", {}, "1.00 Kb")).rejects.toThrow("The read of res.partner.image_128 did not answer a list of records.");
    let fail = true;
    const flaky = client(async () => { if (fail) throw new Error("Disconnected"); return [{ id: 9, image_128: PNG }]; });
    await expect(binaryContent(flaky.rpc, "res.partner", 9, "image_128", {}, "1.00 Kb")).rejects.toThrow("Disconnected");
    // a failed read is not kept: the next request reads again
    fail = false;
    expect(await binaryContent(flaky.rpc, "res.partner", 9, "image_128", {}, "1.00 Kb")).toBe(PNG);
  });
});

describe("image widget on a bin_size value", () => {
  test("reads the preview field of the saved record and shows it; download reads the field itself", async () => {
    const { rpc, call } = client(async (_model, _method, args) => [{ id: 7, [(args["fields"] as string[])[0]!]: (args["fields"] as string[])[0] === "image_128" ? SMALL : PNG }]);
    const handle = mount(rpc, imageField({ preview_image: { name: "image_128", model: "res.partner" } }), { id: 7, image_1920: "2.50 Mb", image_128: "3.10 Kb" });
    const root = handle.node.querySelector(".field-image")!;
    // until the picture arrives the size stands in for it
    expect(root.getAttribute("aria-busy")).toBe("true");
    expect(handle.node.querySelector(".field-image-size")!.textContent).toBe("2.50 Mb");
    await tick();
    expect(root.hasAttribute("aria-busy")).toBe(false);
    expect(shown(handle.node)).toBe(`data:image/png;base64,${SMALL}`);
    expect(call).toHaveBeenCalledTimes(1);
    expect(call.mock.calls[0]![2]).toEqual({ ids: [7], fields: ["image_128"], context: { lang: "en_US", bin_size: false } });
    // Replace and Clear are offered: the record holds a picture
    expect(handle.node.querySelector(".o_select_file_button")!.textContent).toBe("Replace");
    expect(handle.node.querySelector<HTMLElement>(".o_clear_file_button")!.hidden).toBe(false);

    const download = handle.node.querySelector<HTMLAnchorElement>(".o_download_file_button")!;
    expect(download.hidden).toBe(false);
    vi.mocked(saveFile).mockClear();
    download.click(); download.click();
    // the press is shown while the field is read, and a second press before the answer is ignored
    expect(root.getAttribute("aria-busy")).toBe("true");
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Reading Image to download…");
    await tick();
    expect(call).toHaveBeenCalledTimes(2);
    expect(call.mock.calls[1]![2]).toEqual({ ids: [7], fields: ["image_1920"], context: { lang: "en_US", bin_size: false } });
    expect(root.hasAttribute("aria-busy")).toBe(false);
    expect(saveFile).toHaveBeenCalledTimes(1);
    const [blob, name] = vi.mocked(saveFile).mock.calls[0]!;
    expect(name).toBe("image_1920.png");
    expect(blob.type).toBe("image/png");
    expect(blob.size).toBe(atob(PNG).length);
    // content is known now: the next press saves at once, without a read
    download.click();
    expect(saveFile).toHaveBeenCalledTimes(2);
    expect(call).toHaveBeenCalledTimes(2);
    handle.dispose();
  });

  test("without a preview field it reads the field itself, once for display and download", async () => {
    const { rpc, call } = client(async () => [{ id: 7, image_1920: PNG }]);
    const handle = mount(rpc, imageField({}, { readonly: true }), { id: 7, image_1920: "2.50 Mb" }, { model: "res.partner", viewContext: "list" });
    await tick();
    expect(shown(handle.node)).toBe(`data:image/png;base64,${PNG}`);
    expect(handle.node.querySelector<HTMLAnchorElement>(".o_download_file_button")!.getAttribute("href")).toBe(`data:image/png;base64,${PNG}`);
    expect(call).toHaveBeenCalledTimes(1);
    handle.dispose();
  });

  test("cells of one list column are read together", async () => {
    const { rpc, call } = client(async (_model, _method, args) => (args["ids"] as number[]).map(id => ({ id, image_1920: PNG })));
    const handles = [3, 4, 5].map(id => mount(rpc, imageField({}, { readonly: true }), { id, image_1920: "1.00 Kb" }, { model: "product.template", viewContext: "list" }));
    await tick();
    expect(call).toHaveBeenCalledTimes(1);
    expect(call.mock.calls[0]![0]).toBe("product.template");
    expect(call.mock.calls[0]![2]).toMatchObject({ ids: [3, 4, 5], fields: ["image_1920"] });
    for (const handle of handles) { expect(shown(handle.node)).toBe(`data:image/png;base64,${PNG}`); handle.dispose(); }
  });

  test("a failed read keeps the placeholder and says why; a record without identity shows the size only", async () => {
    const { rpc, call } = client(async () => { throw new Error("Disconnected"); });
    const handle = mount(rpc, imageField(), { id: 7, image_1920: "2.50 Mb" });
    await tick();
    expect(shown(handle.node)).toBeUndefined();
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Image could not be loaded: Disconnected");
    expect(handle.node.querySelector(".field-image")!.hasAttribute("aria-busy")).toBe(false);
    document.body.replaceChildren();
    call.mockClear();
    // no model, or a record that is not saved: nothing to read from
    for (const [values, extra] of [[{ id: 7, image_1920: "2.50 Mb" }, {}], [{ image_1920: "2.50 Mb" }, { model: "res.partner" }]] as const) {
      const plain = mount(rpc, imageField(), values, extra);
      await tick();
      expect(plain.node.querySelector(".field-image-size")!.textContent).toBe("2.50 Mb");
      expect(plain.node.querySelector<HTMLElement>(".o_download_file_button")!.hidden).toBe(true);
    }
    expect(call).not.toHaveBeenCalled();
  });

  test("a picture removed after the record was read keeps the placeholder and says so", async () => {
    const { rpc } = client(async () => [{ id: 7, image_1920: false }]);
    const handle = mount(rpc, imageField(), { id: 7, image_1920: "2.50 Mb" });
    await tick();
    expect(shown(handle.node)).toBeUndefined();
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Image is no longer stored on this record. Reload to see its current state.");
    expect(handle.node.querySelector<HTMLElement>(".o_download_file_button")!.hidden).toBe(true);
    handle.dispose();
  });

  test("a widget disposed before the picture arrives stays as it was", async () => {
    let release!: (rows: unknown) => void;
    const { rpc } = client(() => new Promise(resolve => { release = resolve; }));
    const handle = mount(rpc, imageField(), { id: 7, image_1920: "2.50 Mb" });
    await tick();
    handle.dispose();
    release([{ id: 7, image_1920: PNG }]);
    await tick();
    expect(shown(handle.node)).toBeUndefined();
  });
});

describe("image widget: download, enlarge and cancel on a bin_size value", () => {
  test("Download of a file removed meanwhile says so and saves nothing", async () => {
    const { rpc } = client(async () => [{ id: 7, image_1920: false }]);
    const handle = mount(rpc, imageField(), { id: 7, image_1920: "2.50 Mb" });
    await tick();
    vi.mocked(saveFile).mockClear();
    handle.node.querySelector<HTMLAnchorElement>(".o_download_file_button")!.click();
    await tick();
    expect(saveFile).not.toHaveBeenCalled();
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Image is no longer stored on this record. Reload to see its current state.");
    handle.dispose();
  });

  test("enlarging reads the field and shows it only while the pointer is still on the picture", async () => {
    vi.useFakeTimers();
    try {
      const releases: Array<(rows: unknown) => void> = [];
      const { rpc, call } = client(() => new Promise(resolve => { releases.push(resolve); }));
      const handle = mount(rpc, imageField({ zoom: true, preview_image: { name: "image_128", model: "res.partner" } }), { id: 7, image_1920: "2.50 Mb", image_128: "3.10 Kb" });
      await vi.advanceTimersByTimeAsync(0); // the batched read leaves in a microtask
      releases.shift()!([{ id: 7, image_128: SMALL }]);
      await vi.advanceTimersByTimeAsync(0);
      const stage = handle.node.querySelector<HTMLElement>(".field-image-stage")!;
      // pointer leaves before the field arrives: no enlarged view
      stage.dispatchEvent(new Event("mouseenter"));
      await vi.advanceTimersByTimeAsync(0);
      expect(call).toHaveBeenCalledTimes(2);
      stage.dispatchEvent(new Event("mouseleave"));
      releases.shift()!([{ id: 7, image_1920: PNG }]);
      await vi.advanceTimersByTimeAsync(0);
      expect(handle.node.querySelector(".field-image-zoom")).toBeNull();
      // the content is kept now: the next hover enlarges at once
      stage.dispatchEvent(new Event("mouseenter"));
      await vi.advanceTimersByTimeAsync(0);
      expect(handle.node.querySelector(".field-image-zoom img")).not.toBeNull();
      handle.dispose();
    } finally { vi.useRealTimers(); }
  });

  test("a read nobody waits for any more is cancelled; one another widget still waits for is not", async () => {
    const signals: Array<AbortSignal | undefined> = [];
    const { rpc } = client(async (_model, _method, _args, options) => { signals.push(options?.["signal"] as AbortSignal | undefined); return new Promise(() => {}); });
    const first = new AbortController(), second = new AbortController();
    const a = binaryContent(rpc, "res.partner", 1, "image_1920", {}, "1.00 Kb", first.signal);
    void binaryContent(rpc, "res.partner", 1, "image_1920", {}, "1.00 Kb", second.signal);
    await tick();
    first.abort();
    expect(signals[0]!.aborted).toBe(false);
    second.abort();
    expect(signals[0]!.aborted).toBe(true);
    await expect(a).rejects.toMatchObject({ name: "AbortError" });
  });

  test("a caller of the tick its predecessor left in starts a read of its own, not the cancelled one", async () => {
    let reads = 0;
    const { rpc, call } = client(async () => (++reads === 1 ? new Promise(() => {}) : [{ id: 1, image_1920: PNG }]));
    const leaving = new AbortController();
    const first = binaryContent(rpc, "res.partner", 1, "image_1920", {}, "1.00 Kb", leaving.signal);
    await tick();
    // a form render disposes a widget and builds its successor in one tick
    leaving.abort();
    const second = binaryContent(rpc, "res.partner", 1, "image_1920", {}, "1.00 Kb");
    expect(second).not.toBe(first);
    await expect(first).rejects.toMatchObject({ name: "AbortError" });
    expect(await second).toBe(PNG);
    expect(call).toHaveBeenCalledTimes(2);
    // the cancelled read's late rejection does not remove its successor: the content is kept
    await binaryContent(rpc, "res.partner", 1, "image_1920", {}, "1.00 Kb");
    expect(call).toHaveBeenCalledTimes(2);
  });

  test("a widget disposed while its read runs cancels it and shows no error", async () => {
    let signal: AbortSignal | undefined;
    const { rpc } = client(async (_model, _method, _args, options) => { signal = options?.["signal"] as AbortSignal; return new Promise(() => {}); });
    const handle = mount(rpc, imageField(), { id: 7, image_1920: "2.50 Mb" });
    await tick();
    handle.dispose();
    await tick();
    expect(signal!.aborted).toBe(true);
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("");
  });
});

describe("record state and bin_size", () => {
  const fields: Field[] = [
    { name: "name", label: "Name", kind: "char", readonly: false, required: false, relation: null, relationField: null, selection: null },
    { name: "image_1920", label: "Image", kind: "binary", widget: "image", readonly: false, required: false, relation: null, relationField: null, selection: null },
  ];
  function server(saved: Record<string, unknown>) {
    const calls: { path: string; body: Record<string, unknown> }[] = [];
    const rpc = new RpcClient(async (path, init) => {
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      calls.push({ path: String(path), body });
      const answer = String(path).endsWith("onchange") ? { value: {} } : [saved];
      return new Response(JSON.stringify(answer), { headers: { "Content-Type": "application/json" } });
    }, () => {});
    return { rpc, calls };
  }
  const last = (calls: { path: string; body: Record<string, unknown> }[], method: string) => calls.filter(call => call.path.endsWith(method)).at(-1)!.body;

  test("the form reads and saves with bin_size and never sends a size back", async () => {
    const { rpc, calls } = server({ id: 5, name: "Chair", image_1920: "2.50 Mb" });
    const state = new RecordState(rpc, "product.template", fields, { lang: "en_US" });
    await state.load(5);
    expect(last(calls, "web_read")["context"]).toEqual({ lang: "en_US", bin_size: true });
    expect(state.values["image_1920"]).toBe("2.50 Mb");

    state.change("name", "Desk");
    await state.onchange("name");
    // the snapshot names every other field; the stored picture is left to the server
    expect(last(calls, "onchange")["values"]).toEqual({ name: "Desk" });
    expect(last(calls, "onchange")["context"]).toEqual({ lang: "en_US" });
    await state.save();
    expect(last(calls, "web_save")["vals"]).toEqual({ name: "Desk" });
    expect(last(calls, "web_save")["context"]).toEqual({ lang: "en_US", bin_size: true });

    // a new picture is content and is sent; clearing sends null
    state.change("image_1920", PNG);
    await state.onchange("image_1920");
    expect(last(calls, "onchange")["values"]).toEqual({ name: "Chair", image_1920: PNG });
    await state.save();
    expect(last(calls, "web_save")["vals"]).toEqual({ image_1920: PNG });
    state.change("image_1920", false);
    await state.save();
    expect(last(calls, "web_save")["vals"]).toEqual({ image_1920: null });
  });

  test("a line's binary column holding a size is never written back; new content is", async () => {
    const field = (name: string, kind: Field["kind"], extra: Partial<Field> = {}): Field => ({ name, kind, label: name, required: false, readonly: false, relation: null, relationField: null, selection: null, ...extra });
    const title = field("title", "char"), picture = field("image_1920", "binary", { widget: "image" });
    const subview: Subview = { model: "library.book", list: [title, picture], form: { type: "form", mode: "record", fields: [title, picture], nodes: [] }, fields: new Map([[title.name, title], [picture.name, picture]]) };
    const lines = field("book_ids", "one2many", { relation: "library.book", relationField: "author_id", subview });
    const calls: { path: string; body: Record<string, unknown> }[] = [];
    const rpc = new RpcClient(async (path, init) => {
      calls.push({ path: String(path), body: JSON.parse(String(init?.body)) as Record<string, unknown> });
      const book = { id: 2, title: "Earthsea", image_1920: "2.50 Mb" };
      return new Response(JSON.stringify([{ id: 9, name: "Le Guin", book_ids: [book] }]), { headers: { "Content-Type": "application/json" } });
    }, () => {});
    const state = new RecordState(rpc, "library.author", [field("name", "char"), lines], {});
    await state.load(9);
    state.change("book_ids", [{ id: 2, title: "Earthsea 2", image_1920: "2.50 Mb", __changed: ["title", "image_1920"] }, { title: "Lavinia", image_1920: PNG, __key: "line_9" }]);
    await state.save();
    const commands = (calls.filter(call => call.path.endsWith("web_save")).at(-1)!.body["vals"] as Record<string, unknown[][]>)["book_ids"]!;
    expect(commands[0]).toEqual([1, 2, { title: "Earthsea 2" }]);
    expect(commands[1]).toEqual([0, 0, { title: "Lavinia", image_1920: PNG }]);
  });

  test("a view's record read drops loaded content, so a replaced file of the same shown size is read again", async () => {
    const calls: string[] = [];
    const rpc = new RpcClient(async (path, init) => {
      calls.push(String(path).split("/").at(-1)!);
      const reading = String(path).endsWith("/read");
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      return new Response(JSON.stringify(reading ? (body["ids"] as number[]).map(id => ({ id, image_1920: PNG })) : [{ id: 5, name: "Chair", image_1920: "2.50 Mb" }]), { headers: { "Content-Type": "application/json" } });
    }, () => {});
    const state = new RecordState(rpc, "product.template", fields, {});
    await state.load(5);
    await binaryContent(rpc, "product.template", 5, "image_1920", {}, "2.50 Mb");
    await binaryContent(rpc, "product.template", 5, "image_1920", {}, "2.50 Mb");
    expect(calls.filter(call => call === "read")).toHaveLength(1);
    await state.load(5);
    await binaryContent(rpc, "product.template", 5, "image_1920", {}, "2.50 Mb");
    expect(calls.filter(call => call === "read")).toHaveLength(2);
    // a save answers the record again: the same
    state.change("name", "Desk");
    await state.save();
    await binaryContent(rpc, "product.template", 5, "image_1920", {}, "2.50 Mb");
    expect(calls.filter(call => call === "read")).toHaveLength(3);
  });
});
