/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { button, el } from "./dom.js";
import { fieldShell } from "./field-shell.js";
import { icon } from "./icons.js";
import { attachmentRequestCap, humanBytes } from "./attachment-policy.js";
import { binaryContent, isBinarySize } from "./binary-content.js";
import { saveFile } from "./file-save.js";
import { ClientError } from "./transport.js";
import type { FieldControl, FieldControlOptions } from "./fields.js";

/** First base64 character of the content to the image type it starts (the reference sniffs the same five). */
const MAGIC: Readonly<Record<string, string>> = { "/": "jpeg", R: "gif", i: "png", P: "svg+xml", U: "webp" };
/** Types the browser re-encodes to webp; animated and already-webp content is kept as uploaded. */
const KEEP_FORMAT = ["image/gif", "image/webp"];
const DEFAULT_ACCEPT = "image/*";
const BASE64 = /^[A-Za-z0-9+/]*={0,2}$/;
/** Room left in the write request for the record's other changed values. */
const REQUEST_MARGIN = 4096;

/** The `src` of base64 content: a data URL typed from its first character (png when unknown). */
export function imageDataUrl(content: string): string {
  return `data:image/${MAGIC[content.charAt(0)] ?? "png"};base64,${content}`;
}

/** Only http(s) and same-origin paths reach `img.src`; anything else (javascript:, data:, file:) is refused. */
function safeImageUrl(raw: string, writeDate: unknown, reload: boolean): string | null {
  let url: URL;
  try { url = new URL(raw, document.baseURI); } catch { return null; }
  if (url.protocol !== "http:" && url.protocol !== "https:") return null;
  // reload (opt-in): the record's write_date busts the browser cache when the picture behind a fixed URL changes
  if (reload && typeof writeDate === "string" && writeDate) url.searchParams.set("unique", writeDate);
  return url.href;
}

/** Whether `file` matches one entry of an `accept`-style list: `.ext`, `type/sub` or `type/*`. */
export function fileAccepted(file: { name: string; type: string }, accept: string): boolean {
  const name = file.name.toLowerCase(), type = file.type.toLowerCase();
  return accept.split(",").map(entry => entry.trim().toLowerCase()).filter(Boolean).some(entry =>
    entry.startsWith(".") ? name.endsWith(entry) : entry.endsWith("/*") ? type.startsWith(entry.slice(0, -1)) : type === entry);
}

/** A record value of a binary field: empty, base64 content or the `bin_size` text; anything else is refused. */
function binaryValue(field: string, value: unknown): { content: string; size?: string } {
  if (value === false || value === null || value === undefined || value === "") return { content: "" };
  if (isBinarySize(value)) return { content: "", size: value };
  if (typeof value === "string" && value.length % 4 === 0 && BASE64.test(value)) return { content: value };
  throw new ClientError("server", `Field ${field} holds a value that is neither picture content nor a size.`);
}

/** Re-encode to webp through a canvas; a browser that cannot is refused by name, never stored as another type. */
async function toWebp(base64: string, file: File): Promise<string> {
  const image = new Image();
  const loaded = new Promise<void>((resolve, reject) => { image.onload = () => resolve(); image.onerror = () => reject(new ClientError("validation", `"${file.name}" could not be opened as a picture.`)); });
  image.src = `data:${file.type};base64,${base64}`;
  await loaded;
  const unsupported = new ClientError("validation", `This browser cannot convert "${file.name}" to WebP; the field keeps its current picture.`);
  const canvas = document.createElement("canvas");
  canvas.width = image.naturalWidth; canvas.height = image.naturalHeight;
  const context = canvas.getContext("2d");
  if (!context || !canvas.width || !canvas.height) throw unsupported;
  context.drawImage(image, 0, 0);
  const url = canvas.toDataURL("image/webp"), prefix = "data:image/webp;base64,";
  // a browser without a webp encoder answers png; a canvas over its area limit answers "data:,"
  if (!url.startsWith(prefix) || url.length === prefix.length) throw unsupported;
  return url.slice(prefix.length);
}

/**
 * `image` (Binary/Image field) and `image_url` (Char URL field) widgets. The value is base64 content, the
 * `bin_size` text or false. Options: alt, img_class, size [w,h], zoom, zoom_delay, reload, preview_image,
 * accepted_file_extensions, convert_to_webp. Editing adds upload, replace, clear; every state also offers
 * download. The widget never shows a value the record does not hold: a staged change appears when the form
 * renders it again. A value read with `bin_size` is a size only: the widget then reads the picture of its saved
 * record itself (`binary-content.ts`), the smaller `preview_image` field for display and the field's own content
 * when the user enlarges or downloads it; without a saved record it shows the size.
 */
export function imageControl(options: FieldControlOptions): FieldControl {
  const { field, rpc, change } = options, config = field.options ?? {};
  const urlMode = field.widget === "image_url";
  if (field.kind !== (urlMode ? "char" : "binary")) throw new ClientError("server", `Field ${field.name} with ${field.widget} widget must be a ${urlMode ? "char" : "binary"} field.`);
  const editable = !urlMode && !field.readonly && options.viewContext !== "list" && options.viewContext !== "card";
  const accept = config.accepted_file_extensions ?? DEFAULT_ACCEPT;
  if (urlMode && options.value !== false && options.value !== null && options.value !== undefined && typeof options.value !== "string") throw new ClientError("server", `Field ${field.name} holds a value that is not a URL.`);
  const value: { content: string; size?: string } = urlMode ? { content: typeof options.value === "string" ? options.value : "" } : binaryValue(field.name, options.value);
  const current = value.content, sizeText = value.size;

  const root = el("div", undefined, `field-image field-image-${options.viewContext ?? "form"} o_field_image`);
  root.setAttribute("role", "group");
  const shell = fieldShell(field, root, options.viewContext);
  const stage = el("div", undefined, "field-image-stage");
  const width = config.size?.[0] ?? 0, height = config.size?.[1] ?? 0;
  if (width) stage.style.setProperty("--image-w", `${width}px`);
  if (height) stage.style.setProperty("--image-h", `${height}px`);
  stage.classList.toggle("has-width", !!width); stage.classList.toggle("has-height", !!height);
  const actions = el("div", undefined, "field-image-actions");
  const status = el("span", undefined, "field-image-status"); status.setAttribute("role", "status");

  // The preview is the record's smaller copy of the saved picture; an unsaved change shows itself.
  const previewName = config.preview_image?.name;
  const previewValue = previewName === undefined || options.changed ? { content: "" } : binaryValue(previewName, options.values?.[previewName]);
  const preview = previewValue.content;
  // Size-only value of a saved record: the content is read on demand (`lazyFull` the field, `lazySmall` its preview).
  const recordId = options.values?.["id"], recordModel = options.model;
  const target = !urlMode && sizeText !== undefined && recordModel !== undefined && typeof recordId === "number" && Number.isInteger(recordId) && recordId > 0
    ? { model: recordModel, id: recordId, size: sizeText } : undefined;
  const lazy = target !== undefined;
  let lazyFull = "", lazySmall = "", fullRead: Promise<string> | undefined;
  // the widget's lifetime: a read nobody waits for any more is cancelled (`dispose`)
  const life = new AbortController();
  const contentOf = (name: string, size: string): Promise<string> => {
    if (!target) return Promise.reject(new ClientError("validation", `${field.label} has no saved record to read from.`));
    return binaryContent(rpc, target.model, target.id, name, options.context, size, life.signal);
  };
  const sourceOf = (small: boolean): string | null => {
    if (urlMode) return current ? safeImageUrl(current, options.values?.["write_date"], config.reload === true) : null;
    const full = current || lazyFull, little = preview || lazySmall;
    if (!full && !(lazy && small && little)) return null;
    return imageDataUrl(small && little ? little : full);
  };

  let zoomTimer: ReturnType<typeof setTimeout> | undefined;
  let zoomNode: HTMLElement | undefined;
  let zoomWanted = false;
  const hideZoom = () => { zoomWanted = false; clearTimeout(zoomTimer); zoomNode?.remove(); zoomNode = undefined; };
  const showZoom = () => {
    zoomWanted = true;
    clearTimeout(zoomTimer);
    zoomTimer = setTimeout(() => {
      const src = sourceOf(false);
      // size-only value: read the picture, then enlarge it if the pointer or the focus is still here
      if (!src && lazy && !zoomNode) { readFull().then(content => { if (content && zoomWanted && !dead) showZoom(); }, loadFailed); return; }
      if (!src || zoomNode) return;
      zoomNode = el("div", undefined, "field-image-zoom");
      const big = el("img"); big.src = src; big.alt = "";
      zoomNode.append(big); stage.append(zoomNode);
    }, config.zoom_delay ?? 0);
  };

  // One read at a time: a newer pick, Clear or disposal (Discard, pager, breadcrumb) drops the older read.
  let dead = false, request = 0, cancelRead: (() => void) | undefined;
  const stale = (ticket: number) => dead || ticket !== request;
  const cancel = () => { request++; cancelRead?.(); cancelRead = undefined; };
  const unreadable = (file: File) => new ClientError("validation", `"${file.name}" could not be read.`);
  const read = (file: File, ticket: number): Promise<string | undefined> => new Promise((resolve, reject) => {
    const reader = new FileReader();
    const finish = () => { reader.onload = null; reader.onerror = null; reader.onabort = null; cancelRead = undefined; };
    cancelRead = () => { finish(); if (reader.readyState === FileReader.LOADING) reader.abort(); resolve(undefined); };
    reader.onload = () => {
      const text = String(reader.result), comma = text.indexOf(","); finish();
      if (stale(ticket)) resolve(undefined); else if (comma < 0) reject(unreadable(file)); else resolve(text.slice(comma + 1));
    };
    reader.onerror = () => { finish(); reject(unreadable(file)); };
    reader.onabort = () => { finish(); resolve(undefined); };
    reader.readAsDataURL(file);
  });

  const input = editable ? el("input") : null;
  if (input) { input.type = "file"; input.accept = accept; input.hidden = true; input.tabIndex = -1; input.name = field.name; }
  const upload = editable ? button("Upload", () => input?.click(), "field-image-button o_select_file_button") : null;
  const clear = editable ? button("Clear", () => { cancel(); setPending(false); shell.clear(); status.textContent = ""; change(false); }, "field-image-button o_clear_file_button") : null;
  const download = el("a", undefined, "field-image-button o_download_file_button");
  const verb = current || sizeText ? "Replace" : "Upload";
  // Each action holds its icon and its text: a roomy place shows the text, a picture in a form's title block
  // or a list cell shows the icon (client.css); the accessible name is the same in both.
  for (const [node, text, glyph] of [[upload, verb, "camera"], [clear, "Clear", "trash"], [download, "Download", "save"]] as const) {
    if (!node) continue;
    node.replaceChildren(icon(glyph), el("span", text)); node.setAttribute("aria-label", `${text} ${field.label}`); node.title = `${text} ${field.label}`;
  }
  const setPending = (pending: boolean) => {
    if (upload) upload.disabled = pending;
    root.toggleAttribute("aria-busy", pending);
    status.classList.toggle("is-pending", pending);
    status.textContent = pending ? `Reading the picture for ${field.label}…` : "";
  };

  const placeholder = () => {
    const holder = el("div", undefined, "field-image-placeholder"); holder.setAttribute("role", "img");
    holder.setAttribute("aria-label", sizeText ? `${field.label}, ${sizeText}` : `No ${field.label}`);
    holder.append(icon("image"));
    if (sizeText) holder.append(el("span", sizeText, "field-image-size"));
    return holder;
  };
  const showPlaceholder = () => {
    hideZoom(); stage.replaceChildren(placeholder());
    // an empty stage is not a focus stop and does not promise a larger view
    stage.removeAttribute("tabindex"); stage.removeAttribute("aria-label");
    stage.onmouseenter = stage.onmouseleave = stage.onfocus = stage.onblur = null; stage.onkeydown = null;
  };
  const alt = config.alt ?? field.label;
  const showImage = (source: string) => {
    const image = el("img", undefined, `field-image-img${config.img_class ? ` ${config.img_class}` : ""}`); image.src = source; image.alt = alt; image.loading = "lazy";
    if (width) image.width = width; if (height) image.height = height;
    // a broken source degrades to the placeholder and says so; the record keeps its value, so Clear stays
    image.addEventListener("error", () => { showPlaceholder(); download.hidden = true; status.textContent = `${field.label} could not be loaded.`; }, { once: true });
    stage.replaceChildren(image);
    if (config.zoom) {
      stage.tabIndex = 0; stage.setAttribute("aria-label", `${alt} (hover or focus to enlarge)`);
      stage.onmouseenter = showZoom; stage.onmouseleave = hideZoom; stage.onfocus = showZoom; stage.onblur = hideZoom;
      stage.onkeydown = event => { if (event.key === "Escape" && zoomNode) { event.stopPropagation(); hideZoom(); } };
    }
  };
  const full = sourceOf(true);
  if (full) showImage(full); else showPlaceholder();
  if (clear) clear.hidden = !current && !sizeText;
  const offerDownload = (href: string, content: string) => {
    download.hidden = false; download.href = href;
    download.download = urlMode ? "" : `${field.name}.${(MAGIC[content.charAt(0)] ?? "png").replace("svg+xml", "svg").replace("jpeg", "jpg")}`;
    if (urlMode) { download.target = "_blank"; download.rel = "noopener noreferrer"; }
  };
  const href = sourceOf(false);
  download.hidden = !href;
  if (href) offerDownload(href, current);

  // The record keeps its value when a read fails, so the placeholder, Replace and Clear stay; the reason is shown.
  const loadFailed = (error: unknown) => {
    // a read cancelled by disposal is not a failure
    if (dead || (error instanceof DOMException && error.name === "AbortError")) return;
    root.removeAttribute("aria-busy");
    status.textContent = `${field.label} could not be loaded: ${error instanceof Error ? error.message : String(error)}`;
  };
  /** The field's own content, read once; `""` when the record no longer holds a picture. */
  const readFull = (): Promise<string> => fullRead ??= contentOf(field.name, target?.size ?? "").then(content => {
    lazyFull = content;
    if (dead) return content;
    if (content) offerDownload(imageDataUrl(content), content); else download.hidden = true;
    return content;
  }, (error: unknown) => { fullRead = undefined; throw error; });
  const goneText = `${field.label} is no longer stored on this record. Reload to see its current state.`;
  if (lazy) {
    const small = previewName !== undefined && previewValue.size !== undefined ? { name: previewName, size: previewValue.size } : undefined;
    root.setAttribute("aria-busy", "true");
    (small ? contentOf(small.name, small.size).then(content => { lazySmall = content; return content; }) : readFull()).then(content => {
      if (dead) return;
      root.removeAttribute("aria-busy");
      const source = content ? sourceOf(true) : null;
      if (source) showImage(source);
      // the file was removed after the record was read: say so, the size shown is no longer true
      else status.textContent = goneText;
    }, loadFailed);
    download.hidden = false;
    if (!download.hasAttribute("href")) download.href = "#";
  }
  // Download saves through an object URL (a data: link of a large picture can exceed what a browser accepts).
  // A size-only value reads the field itself on the first press; a press while that read runs is ignored.
  let saving = false;
  const saveContent = (content: string) => {
    try {
      const bytes = Uint8Array.from(atob(content), char => char.charCodeAt(0));
      saveFile(new Blob([bytes], { type: `image/${MAGIC[content.charAt(0)] ?? "png"}` }), download.download);
    } catch { status.textContent = `${field.label} could not be saved.`; }
  };
  if (!urlMode) download.addEventListener("click", event => {
    event.preventDefault();
    const content = current || lazyFull;
    if (content) { saveContent(content); return; }
    if (!lazy || saving) return;
    saving = true;
    root.setAttribute("aria-busy", "true"); status.textContent = `Reading ${field.label} to download…`;
    readFull().then(read => {
      if (dead) return;
      root.removeAttribute("aria-busy"); status.textContent = "";
      if (read) saveContent(read); else status.textContent = goneText;
    }, loadFailed).finally(() => { saving = false; });
  });

  input?.addEventListener("change", () => {
    const file = input.files?.[0]; input.value = "";
    if (!file || dead) return;
    shell.clear(); status.textContent = "";
    const type = file.type.toLowerCase();
    if (type === "image/svg+xml" || file.name.toLowerCase().endsWith(".svg")) { shell.invalid(`"${file.name}" is an SVG file; SVG pictures are not accepted. Upload a PNG, JPEG, GIF or WebP file.`); return; }
    if (!fileAccepted(file, accept)) {
      shell.invalid(accept === DEFAULT_ACCEPT ? `"${file.name}" is not a picture. Upload a PNG, JPEG, GIF or WebP file.` : `"${file.name}" (${file.type || "unknown type"}) is not accepted here; accepted: ${accept}.`);
      return;
    }
    // The content travels as base64 inside the record's write, so the request body limit bounds it.
    const cap = attachmentRequestCap(rpc);
    const encodedLimit = cap === undefined ? Infinity : Math.floor((cap - REQUEST_MARGIN) / 4) * 4;
    const tooLarge = () => new ClientError("validation", `"${file.name}" is ${humanBytes(file.size)}; a picture saved with the form can be up to ${humanBytes(encodedLimit / 4 * 3)}.`);
    if (Math.ceil(file.size / 3) * 4 > encodedLimit) { shell.invalid(tooLarge().message); return; }
    options.run(async () => {
      if (dead) return;
      cancel(); const ticket = request;
      setPending(true);
      try {
        let content = await read(file, ticket);
        if (content === undefined || stale(ticket)) return;
        if (config.convert_to_webp && !KEEP_FORMAT.includes(type)) { content = await toWebp(content, file); if (stale(ticket)) return; }
        if (content.length > encodedLimit) throw tooLarge();
        setPending(false);
        change(content);
      } catch (error) {
        if (stale(ticket)) return;
        setPending(false);
        // field-level refusals stay with the field; session, transport and server errors take the app's path
        if (error instanceof ClientError && error.kind === "validation") shell.invalid(error.message); else throw error;
      }
    });
  });

  for (const node of [upload, clear, download]) if (node) actions.append(node);
  root.append(stage, ...(input ? [input] : []), actions, status);
  return { node: shell.node, dispose: () => { dead = true; life.abort(); cancel(); hideZoom(); } };
}
