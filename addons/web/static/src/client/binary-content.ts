/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { ClientError, type RpcClient } from "./transport.js";

/**
 * Content of binary fields that a `bin_size` read left out.
 *
 * Views read their records with `bin_size` (`BIN_SIZE_CONTEXT`), so a list, a card or a form carries the size
 * of a file, not the file. A widget that draws a picture asks here for the one field it draws. Requests made
 * in one tick for the same model and field travel as one `read`; a loaded value is kept until the next
 * record read of a view (`forgetBinaryContent`), so a form that renders again does not read it again.
 */

/** Context keys a view adds to its record reads: binary fields answer their size, not their content. */
export const BIN_SIZE_CONTEXT: Readonly<Record<string, unknown>> = { bin_size: true };

/** The `bin_size` text of the server (`erp_orm::image::human_size`): two decimals and a unit. */
const BIN_SIZE_TEXT = /^\d+\.\d{2} (bytes|Kb|Mb|Gb|Tb)$/;
/** Whether `value` is the size text a `bin_size` read returns in place of content. */
export function isBinarySize(value: unknown): value is string {
  return typeof value === "string" && BIN_SIZE_TEXT.test(value);
}

/**
 * Characters of loaded content kept at most (base64 text). A bound on this tab's memory, not a server
 * limit: past it the oldest entries go and are read again when a widget asks.
 */
const KEPT_CHARACTERS = 16 * 1024 * 1024;

/** `users`: callers that wait for the entry without a lifetime signal or whose signal has not aborted; `cancel` drops it unread. */
interface Entry { promise: Promise<string>; characters: number; users: number; settled: boolean; cancel(): void }
interface Waiter { resolve(content: string): void; reject(error: unknown): void }
interface Batch { model: string; field: string; context: Record<string, unknown>; waiters: Map<number, Waiter[]>; abort: AbortController }
interface Store { entries: Map<string, Entry>; characters: number; batches: Map<string, Batch> }

const stores = new WeakMap<RpcClient, Store>();
function storeOf(rpc: RpcClient): Store {
  let store = stores.get(rpc);
  if (!store) { store = { entries: new Map(), characters: 0, batches: new Map() }; stores.set(rpc, store); }
  return store;
}

/** Drop every loaded value: a view calls this when it reads its records, so a changed file is read again. */
export function forgetBinaryContent(rpc: RpcClient): void {
  const store = stores.get(rpc);
  if (!store) return;
  store.entries.clear(); store.characters = 0;
}

async function flush(rpc: RpcClient, batch: Batch): Promise<void> {
  const ids = [...batch.waiters.keys()];
  if (ids.length === 0) return;
  const settle = (id: number, act: (waiter: Waiter) => void) => { for (const waiter of batch.waiters.get(id) ?? []) act(waiter); };
  try {
    const rows = await rpc.call(batch.model, "read", { ids, fields: [batch.field], context: { ...batch.context, bin_size: false } }, { background: true, signal: batch.abort.signal });
    if (!Array.isArray(rows)) throw new ClientError("server", `The read of ${batch.model}.${batch.field} did not answer a list of records.`);
    const found = new Map<number, unknown>();
    for (const row of rows) {
      const record = row as Record<string, unknown> | null;
      const id = record !== null && typeof record === "object" ? record["id"] : undefined;
      if (typeof id !== "number") throw new ClientError("server", `The read of ${batch.model}.${batch.field} answered a row without an id.`);
      found.set(id, record![batch.field]);
    }
    for (const id of ids) {
      if (!found.has(id)) { settle(id, waiter => waiter.reject(new ClientError("server", `${batch.model} record ${id} is no longer available; ${batch.field} was not read.`))); continue; }
      const value = found.get(id);
      // false: the file was removed after the size was read
      if (value === false || value === null) settle(id, waiter => waiter.resolve(""));
      else if (typeof value === "string" && !isBinarySize(value)) settle(id, waiter => waiter.resolve(value));
      else settle(id, waiter => waiter.reject(new ClientError("server", `The read of ${batch.model}.${batch.field} answered no content for record ${id}.`)));
    }
  } catch (error) {
    // every waiter left and was told already
    if (batch.abort.signal.aborted) return;
    for (const id of ids) settle(id, waiter => waiter.reject(error));
  }
}

/**
 * The base64 content of `model` record `id`'s binary `field`, `""` when it holds none. `size` is the size text
 * the view's read returned: a value of another size is another file and is read on its own. `signal` is the
 * caller's lifetime: when every caller of one read has aborted, the read is cancelled and rejects with an
 * `AbortError`.
 */
export function binaryContent(rpc: RpcClient, model: string, id: number, field: string, context: Readonly<Record<string, unknown>>, size: string, signal?: AbortSignal): Promise<string> {
  const store = storeOf(rpc);
  const key = `${model}\n${field}\n${id}\n${size}`;
  const kept = store.entries.get(key);
  if (kept) { attach(kept, signal); return kept.promise; }
  const batchKey = `${model}\n${field}\n${JSON.stringify(context)}`;
  let batch = store.batches.get(batchKey);
  if (!batch) {
    const created: Batch = { model, field, context: { ...context }, waiters: new Map(), abort: new AbortController() };
    batch = created;
    store.batches.set(batchKey, created);
    queueMicrotask(() => { store.batches.delete(batchKey); void flush(rpc, created); });
  }
  const waiters = batch.waiters;
  const abort = batch.abort;
  let mine!: Waiter;
  const entry: Entry = {
    characters: 0, users: 0, settled: false,
    promise: new Promise<string>((resolve, reject) => { mine = { resolve, reject }; waiters.set(id, [...(waiters.get(id) ?? []), mine]); }),
    cancel() {
      // gone at once, not one microtask later in the rejection handler: a caller of the same tick starts a read of its own
      if (store.entries.get(key) === entry) store.entries.delete(key);
      const rest = (waiters.get(id) ?? []).filter(waiter => waiter !== mine);
      if (rest.length > 0) waiters.set(id, rest); else waiters.delete(id);
      mine.reject(new DOMException(`The read of ${model}.${field} was cancelled.`, "AbortError"));
      // an empty batch is closed to newcomers: they would join a read that is already cancelled
      if (waiters.size === 0) { abort.abort(); if (store.batches.get(batchKey) === batch) store.batches.delete(batchKey); }
    },
  };
  attach(entry, signal);
  store.entries.set(key, entry);
  entry.promise.then(content => {
    entry.settled = true;
    if (store.entries.get(key) !== entry) return;
    entry.characters = content.length; store.characters += content.length;
    for (const [oldest, item] of store.entries) {
      if (store.characters <= KEPT_CHARACTERS) break;
      // an entry still loading weighs nothing yet; the newest one stays even when it alone is over the bound
      if (item === entry || item.characters === 0) continue;
      store.entries.delete(oldest); store.characters -= item.characters;
    }
  }, () => { entry.settled = true; if (store.entries.get(key) === entry) store.entries.delete(key); });
  return entry.promise;
}

/** Count a caller of `entry`; the read is cancelled when its last caller has aborted. A caller without a signal never leaves. */
function attach(entry: Entry, signal: AbortSignal | undefined): void {
  entry.users++;
  if (!signal) return;
  const leave = () => { if (--entry.users === 0 && !entry.settled) entry.cancel(); };
  if (signal.aborted) leave(); else signal.addEventListener("abort", leave, { once: true });
}
