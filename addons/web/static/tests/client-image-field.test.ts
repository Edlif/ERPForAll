/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { afterEach, describe, expect, test, vi } from "vitest";
import { createFieldControl } from "../src/client/fields.js";
import { initializeAttachmentPolicy } from "../src/client/attachment-policy.js";
import { readFieldOptions } from "../src/client/field-options.js";
import { fileAccepted, imageDataUrl } from "../src/client/image-field.js";
import type { Field } from "../src/client/metadata.js";
import { ClientError, type RpcClient } from "../src/client/transport.js";

const css = readFileSync(resolve(process.cwd(), "..", "addons/web/static/src/client/client.css"), "utf8");
const PNG = "iVBORw0KGgo=";
const JPEG = "/9j/4AAQ";

function makeField(widget: string, kind: Field["kind"], options: Record<string, unknown> = {}, extra: Partial<Field> = {}): Field {
  return { name: "logo", label: "Logo", kind, widget, options, readonly: false, required: false, relation: null, relationField: null, selection: null, ...extra };
}
interface MountOptions { values?: Record<string, unknown>; viewContext?: "form" | "list" | "card"; changed?: boolean; requestCap?: number; refuseRun?: boolean; change?: (value: unknown) => void }
function mount(field: Field, value: unknown, { values = {}, viewContext, changed, requestCap, refuseRun, change: changeImpl }: MountOptions = {}) {
  const change = vi.fn(changeImpl ?? (() => {})), call = vi.fn(async () => ({})), jobs: Promise<void>[] = [], errors: unknown[] = [];
  const rpc = { call } as unknown as RpcClient;
  if (requestCap !== undefined) initializeAttachmentPolicy(rpc, null, requestCap);
  // refuseRun stands for the app's busy refusal: the job never starts
  const run = (job: () => Promise<void>) => { if (!refuseRun) jobs.push(job().catch(error => { errors.push(error); })); };
  const handle = createFieldControl({ field, value, values, rpc, context: {}, change, run, ...(viewContext ? { viewContext } : {}), ...(changed === undefined ? {} : { changed }) });
  document.body.append(handle.node);
  return { handle, change, call, errors, settle: () => Promise.all(jobs) };
}
function pick(node: HTMLElement, file: File): void {
  const input = node.querySelector<HTMLInputElement>("input[type=file]")!;
  Object.defineProperty(input, "files", { value: [file], configurable: true });
  input.dispatchEvent(new Event("change"));
}
const png = (name = "logo.png", bytes = 4) => new File([new Uint8Array(bytes).fill(137)], name, { type: "image/png" });
const imageSrc = (node: HTMLElement) => node.querySelector<HTMLImageElement>(".field-image-stage img")?.src;
const fieldError = (node: HTMLElement) => node.querySelector(".field-error")!.textContent;

/** A FileReader whose reads finish only when the test says so. */
class HeldReader {
  static readonly LOADING = 1;
  static readers: HeldReader[] = [];
  readyState = 0; result: string | null = null; aborted = false;
  onload: (() => void) | null = null; onerror: (() => void) | null = null; onabort: (() => void) | null = null;
  readAsDataURL(): void { this.readyState = 1; HeldReader.readers.push(this); }
  abort(): void { this.aborted = true; this.readyState = 2; this.onabort?.(); }
  finish(base64: string): void { this.readyState = 2; this.result = `data:image/png;base64,${base64}`; this.onload?.(); }
}
function holdReads(): typeof HeldReader { HeldReader.readers = []; vi.stubGlobal("FileReader", HeldReader); return HeldReader; }

function stubCanvas(dataUrl: string): void {
  vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockReturnValue({ drawImage: vi.fn() } as unknown as CanvasRenderingContext2D);
  vi.spyOn(HTMLCanvasElement.prototype, "toDataURL").mockReturnValue(dataUrl);
  vi.stubGlobal("Image", class { naturalWidth = 2; naturalHeight = 2; onload: (() => void) | null = null; onerror: (() => void) | null = null; set src(_: string) { queueMicrotask(() => this.onload?.()); } });
}

afterEach(() => { document.body.replaceChildren(); vi.useRealTimers(); vi.unstubAllGlobals(); vi.restoreAllMocks(); });

describe("image widget (IMG.WIDGET)", () => {
  test("types the data URL from the first base64 character", () => {
    expect(imageDataUrl(PNG)).toBe(`data:image/png;base64,${PNG}`);
    expect(imageDataUrl(JPEG)).toContain("data:image/jpeg;");
  });

  test("an empty field shows a labelled placeholder and no clear or download button", () => {
    const { handle } = mount(makeField("image", "binary"), false);
    expect(handle.node.querySelector("img")).toBeNull();
    expect(handle.node.querySelector("[role=img]")?.getAttribute("aria-label")).toBe("No Logo");
    expect(handle.node.querySelector<HTMLElement>(".o_clear_file_button")!.hidden).toBe(true);
    expect(handle.node.querySelector<HTMLElement>(".o_download_file_button")!.hidden).toBe(true);
    expect(handle.node.querySelector(".o_select_file_button")?.getAttribute("aria-label")).toBe("Upload Logo");
  });

  test("alt, img_class and size options reach the image", () => {
    const { handle } = mount(makeField("image", "binary", { alt: "Company logo", img_class: "rounded", size: [0, 90] }), PNG);
    const image = handle.node.querySelector("img")!;
    expect(image.alt).toBe("Company logo");
    expect(image.classList.contains("rounded")).toBe(true);
    expect(handle.node.querySelector(".field-image")!.classList.contains("rounded")).toBe(false);
    expect(image.getAttribute("height")).toBe("90");
    expect(image.hasAttribute("name")).toBe(false);
    expect(handle.node.querySelector<HTMLElement>(".field-image-stage")!.style.getPropertyValue("--image-h")).toBe("90px");
  });

  test("the size option admits two whole numbers from 0 to 4096 only", () => {
    expect(readFieldOptions({ size: [0, 90] }, "m", new Map())).toEqual({ size: [0, 90] });
    for (const size of [[90], [1.5, 2], [0, 4097], [-1, 2], "90"]) expect(() => readFieldOptions({ size }, "m", new Map())).toThrow("Invalid field option: size.");
  });

  test("preview_image shows the smaller related field and zoom shows the full record field", () => {
    vi.useFakeTimers();
    const { handle } = mount(makeField("image", "binary", { preview_image: { model: "m", name: "logo_web" }, zoom: true, zoom_delay: 300 }), PNG, { values: { logo_web: JPEG } });
    expect(imageSrc(handle.node)).toContain("data:image/jpeg;");
    const stage = handle.node.querySelector<HTMLElement>(".field-image-stage")!;
    stage.dispatchEvent(new Event("mouseenter"));
    vi.advanceTimersByTime(299);
    expect(handle.node.querySelector(".field-image-zoom")).toBeNull();
    vi.advanceTimersByTime(1);
    expect(handle.node.querySelector<HTMLImageElement>(".field-image-zoom img")!.src).toContain("data:image/png;");
    stage.dispatchEvent(new Event("mouseleave"));
    expect(handle.node.querySelector(".field-image-zoom")).toBeNull();
  });

  test("after an upload the form shows the uploaded picture, not the preview of the saved one", () => {
    const { handle } = mount(makeField("image", "binary", { preview_image: { model: "m", name: "logo_web" } }), PNG, { values: { logo_web: JPEG }, changed: true });
    expect(imageSrc(handle.node)).toBe(`data:image/png;base64,${PNG}`);
  });

  test("zoom is keyboard reachable and Escape closes it without leaving the stage", () => {
    const { handle } = mount(makeField("image", "binary", { zoom: true }), PNG);
    const stage = handle.node.querySelector<HTMLElement>(".field-image-stage")!;
    expect(stage.tabIndex).toBe(0);
    vi.useFakeTimers();
    stage.dispatchEvent(new Event("focus")); vi.advanceTimersByTime(1);
    expect(handle.node.querySelector(".field-image-zoom")).not.toBeNull();
    const outer = vi.fn(); document.body.addEventListener("keydown", outer);
    stage.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(handle.node.querySelector(".field-image-zoom")).toBeNull();
    expect(outer).not.toHaveBeenCalled();
    document.body.removeEventListener("keydown", outer);
  });

  test("a picture that fails to load leaves no focusable enlarge stage and keeps Clear", () => {
    const { handle } = mount(makeField("image", "binary", { zoom: true }), PNG);
    handle.node.querySelector("img")!.dispatchEvent(new Event("error"));
    const stage = handle.node.querySelector<HTMLElement>(".field-image-stage")!;
    expect(stage.hasAttribute("tabindex")).toBe(false); expect(stage.hasAttribute("aria-label")).toBe(false);
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Logo could not be loaded.");
    expect(handle.node.querySelector<HTMLElement>(".o_clear_file_button")!.hidden).toBe(false);
    expect(handle.node.querySelector(".o_select_file_button")!.textContent).toBe("Replace");
  });

  test("upload reads base64 and stages it; the widget keeps the record's value until the form renders again", async () => {
    const { handle, change, call, settle } = mount(makeField("image", "binary"), false);
    pick(handle.node, new File([new Uint8Array([137, 80, 78, 71])], "logo.png", { type: "image/png" }));
    await settle();
    expect(change).toHaveBeenCalledWith("iVBORw==");
    expect(call).not.toHaveBeenCalled();
    expect(handle.node.querySelector("img")).toBeNull();
  });

  test("a file over the request limit is refused with its name, its size and the limit, before it is read", async () => {
    const Reader = holdReads();
    const { handle, change, settle } = mount(makeField("image", "binary"), false, { requestCap: 65536 });
    pick(handle.node, png("big.png", 50000));
    await settle();
    expect(change).not.toHaveBeenCalled(); expect(Reader.readers).toHaveLength(0);
    expect(fieldError(handle.node)).toBe('"big.png" is 48.8 KiB; a picture saved with the form can be up to 45 KiB.');
  });

  test("a read in progress disables Upload and shows a pending state", async () => {
    const Reader = holdReads();
    const { handle, settle } = mount(makeField("image", "binary"), false);
    pick(handle.node, png());
    const upload = handle.node.querySelector<HTMLButtonElement>(".o_select_file_button")!;
    expect(upload.disabled).toBe(true);
    expect(handle.node.querySelector(".field-image")!.hasAttribute("aria-busy")).toBe(true);
    expect(handle.node.querySelector(".field-image-status")!.textContent).toBe("Reading the picture for Logo…");
    Reader.readers[0]!.finish(PNG); await settle();
    expect(upload.disabled).toBe(false);
    expect(handle.node.querySelector(".field-image")!.hasAttribute("aria-busy")).toBe(false);
  });

  test("Discard, the pager or a breadcrumb during a read drops the read", async () => {
    const Reader = holdReads();
    const { handle, change, settle } = mount(makeField("image", "binary"), JPEG);
    pick(handle.node, png());
    handle.dispose();
    expect(Reader.readers[0]!.aborted).toBe(true);
    Reader.readers[0]!.finish(PNG); await settle();
    expect(change).not.toHaveBeenCalled();
  });

  test("a second pick supersedes the first, also when the first finishes last", async () => {
    const Reader = holdReads();
    const { handle, change, settle } = mount(makeField("image", "binary"), false);
    pick(handle.node, png("a.png")); pick(handle.node, png("b.png"));
    const [first, second] = Reader.readers;
    expect(first!.aborted).toBe(true);
    second!.finish("Qg=="); first!.finish("QQ=="); await settle();
    expect(change).toHaveBeenCalledTimes(1); expect(change).toHaveBeenCalledWith("Qg==");
  });

  test("a pick or Clear the app refuses as busy leaves the record's picture on screen", async () => {
    const Reader = holdReads();
    const { handle, change, settle } = mount(makeField("image", "binary"), JPEG, { refuseRun: true });
    pick(handle.node, png()); await settle();
    expect(Reader.readers).toHaveLength(0);
    expect(handle.node.querySelector<HTMLButtonElement>(".o_select_file_button")!.disabled).toBe(false);
    // the app's change refuses with a notice and renders nothing; the widget did not pretend it changed
    handle.node.querySelector<HTMLButtonElement>(".o_clear_file_button")!.click();
    expect(change).toHaveBeenCalledWith(false);
    expect(imageSrc(handle.node)).toContain("data:image/jpeg;");
  });

  test("session, transport and server errors go to the app; only field refusals show as field text", async () => {
    const { handle, errors, settle } = mount(makeField("image", "binary"), false, { change: () => { throw new ClientError("ambiguous", "Session expired."); } });
    pick(handle.node, new File([new Uint8Array([137])], "a.png", { type: "image/png" }));
    await settle();
    expect(errors).toEqual([expect.objectContaining({ kind: "ambiguous", message: "Session expired." })]);
    expect(handle.node.querySelector<HTMLElement>(".field-error")!.hidden).toBe(true);
  });

  test("a refused extension names the file, its type and the accepted list", async () => {
    const { handle, change, settle } = mount(makeField("image", "binary", { accepted_file_extensions: ".png,.jpg" }), false);
    pick(handle.node, new File(["x"], "notes.pdf", { type: "application/pdf" }));
    await settle();
    expect(change).not.toHaveBeenCalled();
    expect(fieldError(handle.node)).toBe('"notes.pdf" (application/pdf) is not accepted here; accepted: .png,.jpg.');
    expect(handle.node.querySelector<HTMLInputElement>("input[type=file]")!.accept).toBe(".png,.jpg");
  });

  test("a file that is not a picture and an SVG picture are refused in plain words", async () => {
    const { handle, change, settle } = mount(makeField("image", "binary"), false);
    pick(handle.node, new File(["x"], "notes.pdf", { type: "application/pdf" }));
    expect(fieldError(handle.node)).toBe('"notes.pdf" is not a picture. Upload a PNG, JPEG, GIF or WebP file.');
    pick(handle.node, new File(["<svg/>"], "logo.svg", { type: "image/svg+xml" }));
    await settle();
    expect(change).not.toHaveBeenCalled();
    expect(fieldError(handle.node)).toBe('"logo.svg" is an SVG file; SVG pictures are not accepted. Upload a PNG, JPEG, GIF or WebP file.');
  });

  test("fileAccepted understands extensions, exact types and wildcards", () => {
    expect(fileAccepted({ name: "A.PNG", type: "" }, ".png")).toBe(true);
    expect(fileAccepted({ name: "a", type: "image/webp" }, "image/*")).toBe(true);
    expect(fileAccepted({ name: "a", type: "image/gif" }, "image/png")).toBe(false);
  });

  test("clear stages false; the placeholder comes with the form's next render", () => {
    const { handle, change } = mount(makeField("image", "binary"), PNG);
    handle.node.querySelector<HTMLButtonElement>(".o_clear_file_button")!.click();
    expect(change).toHaveBeenCalledWith(false);
    const { handle: next } = mount(makeField("image", "binary"), false);
    expect(next.node.querySelector("[role=img]")).not.toBeNull();
  });

  test("download links the content with a typed file name", () => {
    const { handle } = mount(makeField("image", "binary"), JPEG);
    const link = handle.node.querySelector<HTMLAnchorElement>(".o_download_file_button")!;
    expect(link.download).toBe("logo.jpg"); expect(link.getAttribute("href")).toContain("data:image/jpeg;");
  });

  test("the title-block icon actions stay in the tab order: hidden by opacity only, shown on focus and where there is no hover", () => {
    const actions = String.raw`\.form-title-block > \.field-label > \.field-image > \.field-image-actions`;
    const rule = new RegExp(`${actions} \\{[^}]*\\}`).exec(css)?.[0] ?? "";
    expect(rule).toMatch(/opacity: 0;/);
    // display:none and visibility:hidden would drop the buttons from the tab order; the text span is clipped, never removed
    expect(rule).not.toMatch(/display: none|visibility: hidden/);
    expect(css).toMatch(/\.field-image:is\(:hover, :focus-within\) > \.field-image-actions \{ opacity: 1; \}/);
    expect(css).toMatch(/@media \(hover: none\) \{[^}]*\.field-image-actions \{ opacity: 1; \} \}/);
    expect(css).toMatch(/\.field-image-button > span \{ position: absolute;[^}]*clip-path: inset\(50%\)/);
  });

  test("each action holds an icon and its text, and keeps one accessible name", () => {
    const { handle } = mount(makeField("image", "binary"), PNG);
    for (const [selector, text, glyph] of [[".o_select_file_button", "Replace", "camera"], [".o_clear_file_button", "Clear", "trash"], [".o_download_file_button", "Download", "save"]] as const) {
      const action = handle.node.querySelector<HTMLElement>(selector)!;
      expect(action.querySelector("svg")!.getAttribute("data-icon")).toBe(glyph);
      expect(action.querySelector("svg")!.getAttribute("aria-hidden")).toBe("true");
      expect(action.querySelector("span")!.textContent).toBe(text);
      expect(action.getAttribute("aria-label")).toBe(`${text} Logo`); expect(action.title).toBe(`${text} Logo`);
    }
    document.body.replaceChildren();
    const empty = mount(makeField("image", "binary"), false);
    expect(empty.handle.node.querySelector(".o_select_file_button span")!.textContent).toBe("Upload");
  });

  test("readonly and list contexts offer no upload or clear", () => {
    for (const [field, viewContext] of [[makeField("image", "binary", {}, { readonly: true }), undefined], [makeField("image", "binary"), "list" as const]] as const) {
      const { handle } = mount(field, PNG, viewContext ? { viewContext } : {});
      expect(handle.node.querySelector(".o_select_file_button, .o_clear_file_button, input[type=file]")).toBeNull();
      expect(handle.node.querySelector("img")).not.toBeNull();
      document.body.replaceChildren();
    }
  });

  test("a bin_size read shows the placeholder with the size; other values are refused by name", () => {
    const { handle } = mount(makeField("image", "binary", {}, { readonly: true }), "12.30 Kb");
    const holder = handle.node.querySelector("[role=img]")!;
    expect(holder.getAttribute("aria-label")).toBe("Logo, 12.30 Kb"); expect(holder.querySelector(".field-image-size")!.textContent).toBe("12.30 Kb");
    expect(handle.node.querySelector<HTMLElement>(".o_download_file_button")!.hidden).toBe(true);
    for (const value of [{ size: 3 }, 42, "not base64!"]) expect(() => mount(makeField("image", "binary"), value)).toThrow("Field logo holds a value that is neither picture content nor a size.");
    expect(() => mount(makeField("image_url", "char"), { url: "x" })).toThrow("Field logo holds a value that is not a URL.");
  });

  test("required marks the label, not the group", () => {
    const { handle } = mount(makeField("image", "binary", {}, { required: true }), false);
    expect(handle.node.querySelector(".field-image")!.hasAttribute("aria-required")).toBe(false);
    expect(handle.node.querySelector(".field-required-marker")).not.toBeNull();
  });

  test("convert_to_webp re-encodes through a canvas and keeps gif and webp", async () => {
    stubCanvas("data:image/webp;base64,UklGRg==");
    const converted = mount(makeField("image", "binary", { convert_to_webp: true }), false);
    pick(converted.handle.node, new File([new Uint8Array([137])], "a.png", { type: "image/png" }));
    await converted.settle();
    expect(converted.change).toHaveBeenCalledWith("UklGRg==");
    document.body.replaceChildren();
    const gif = mount(makeField("image", "binary", { convert_to_webp: true }), false);
    pick(gif.handle.node, new File([new Uint8Array([71])], "a.gif", { type: "image/gif" }));
    await gif.settle();
    expect(gif.change).toHaveBeenCalledWith("Rw==");
  });

  test.each([["png instead of webp", "data:image/png;base64,iVBORw=="], ["an empty canvas", "data:,"]])("convert_to_webp refuses by name when the browser answers %s, and keeps the old picture", async (_, answer) => {
    stubCanvas(answer);
    const { handle, change, settle } = mount(makeField("image", "binary", { convert_to_webp: true }), JPEG);
    pick(handle.node, new File([new Uint8Array([137])], "a.png", { type: "image/png" }));
    await settle();
    expect(change).not.toHaveBeenCalled();
    expect(fieldError(handle.node)).toBe('This browser cannot convert "a.png" to WebP; the field keeps its current picture.');
    expect(imageSrc(handle.node)).toContain("data:image/jpeg;");
  });

  test("a widget on the wrong field kind fails loudly", () => {
    expect(() => mount(makeField("image", "char"), "x")).toThrow(/must be a binary field/);
  });
});

describe("image_url widget (IMG.WIDGET)", () => {
  test("renders the URL as given by default, so signed URLs keep their signature", () => {
    const { handle } = mount(makeField("image_url", "char", { zoom: true }), "https://cdn.test/a.png?sig=1", { values: { write_date: "2026-11-16 10:00:00" } });
    expect(imageSrc(handle.node)).toBe("https://cdn.test/a.png?sig=1");
    expect(handle.node.querySelector("input[type=file]")).toBeNull();
  });

  test("reload adds the write_date cache key", () => {
    const { handle } = mount(makeField("image_url", "char", { reload: true }), "https://cdn.test/a.png", { values: { write_date: "2026-11-16 10:00:00" } });
    expect(new URL(imageSrc(handle.node)!).searchParams.get("unique")).toBe("2026-11-16 10:00:00");
  });

  test("a javascript: URL never reaches the image", () => {
    const { handle } = mount(makeField("image_url", "char"), "javascript:alert(1)");
    expect(handle.node.querySelector("img")).toBeNull();
  });
});
