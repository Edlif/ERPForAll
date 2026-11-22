/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
export interface AssistantMessage {
  readonly role: "user" | "assistant";
  readonly text: string;
  readonly state: "ready" | "pending" | "error";
  readonly canvas?: unknown;
}

export interface AssistantSession {
  readonly messages: readonly AssistantMessage[];
  readonly busy: boolean;
  readonly persistenceWarning: string;
  send(prompt: string, providerText?: string): Promise<void>;
  subscribe(changed: () => void): () => void;
  subscribeDraft(changed: (text: string) => void): () => void;
  readDock(): DockState;
  updateDock(patch: Partial<DockState>): void;
}

interface DockState { mode: "closed" | "expanded" | "minimized"; draft: string; scroll: number }
interface HistoryPolicy { turns: number; bytes: number; messageBytes: number }
interface Options { userId?: number; request?: typeof fetch; history: HistoryPolicy }
const encoder = new TextEncoder();

export function readHistoryPolicy(raw: unknown): HistoryPolicy {
  if (!record(raw)) throw new Error("Could not load the assistant history limits.");
  function bound(value: unknown, min: number, max: number): number {
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < min || value > max) {
      throw new Error("Invalid assistant history limit.");
    }
    return value;
  }
  return {
    turns: bound(raw["turns"], 1, 200),
    bytes: bound(raw["bytes"], 4096, 2097152),
    messageBytes: bound(raw["message_bytes"], 1, 32768),
  };
}
const sessions = new WeakMap<object, Map<string, AssistantSession>>();
const emptyDock = (): DockState => ({ mode: "closed", draft: "", scroll: 0 });
function record(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
function message(value: unknown): value is AssistantMessage {
  return record(value) && (value["role"] === "user" || value["role"] === "assistant") && typeof value["text"] === "string"
    && ["ready", "pending", "error"].includes(String(value["state"]));
}
function dockState(value: unknown): value is DockState {
  return record(value) && ["closed", "expanded", "minimized"].includes(String(value["mode"])) && typeof value["draft"] === "string"
    && typeof value["scroll"] === "number" && Number.isFinite(value["scroll"]) && value["scroll"] >= 0;
}

/** One live HTTP turn per authenticated shell and company, shared by its dock and workspace. */
export function assistantSession(rpcKey: object, company: number, token: string, options: Options): AssistantSession {
  const cache = sessions.get(rpcKey) ?? new Map<string, AssistantSession>();
  sessions.set(rpcKey, cache);
  const id = (options.userId ?? "") + "." + company;
  const previous = cache.get(id);
  if (previous) return previous;
  const prefix = Number.isSafeInteger(options.userId) && Number(options.userId) > 0 ? "erp.session.ai." + id + "." : null;
  const policy = options.history;
  let warning = "";
  let ended = false;
  function read(suffix: string): unknown {
    if (!prefix) return null;
    try {
      const value = window.sessionStorage.getItem(prefix + suffix);
      if (value === null) return null;
      // A prior release or a lowered setting may have left a larger tab value.
      if (value.length > 2097152) {
        warning = "The previous assistant history exceeds the browser storage limit.";
        return null;
      }
      return JSON.parse(value);
    } catch {
      warning = "This tab could not restore its previous chat.";
      return null;
    }
  }
  function save(suffix: string, value: unknown): void {
    if (!prefix || ended) return;
    try { window.sessionStorage.setItem(prefix + suffix, JSON.stringify(value)); }
    catch { warning = "This browser cannot save the chat for a page reload."; }
  }

  const rawMessages = read("messages");
  let entries: AssistantMessage[] = [];
  if (Array.isArray(rawMessages) && rawMessages.every(message)) {
    entries = rawMessages.map(item => item.state === "pending"
      ? { role: "assistant", state: "error", text: "This tab reloaded before the reply arrived. The provider may have processed the question." } : item);
  } else if (rawMessages !== null) { warning = "This tab could not restore its previous chat."; }
  const rawDock = read("dock");
  let dock = dockState(rawDock) ? rawDock : emptyDock();
  const subscribers = new Set<() => void>();
  const draftSubscribers = new Set<(text: string) => void>();
  let pending: AbortController | undefined;
  let trimmed = read("truncated") === true;
  let currentExchangeTooLarge = false;
  function retainRecent(): void {
    currentExchangeTooLarge = false;
    let start = entries.length;
    let bytes = 2; // brackets around the serialized array
    while (start >= 2 && entries.length - start < policy.turns * 2) {
      const pair = entries.slice(start - 2, start);
      const encoded = JSON.stringify(pair);
      const pairBytes = encoder.encode(encoded).length - 2 + (start < entries.length ? 1 : 0);
      if (bytes + pairBytes > policy.bytes) break;
      bytes += pairBytes;
      start -= 2;
    }
    if (start) {
      trimmed = true;
      save("truncated", true);
      if (start === entries.length) {
        // Keep the current question and reply on screen even if that pair alone exceeds
        // the persistence budget. Replacing the pending reply must still work.
        entries = entries.slice(-2);
        save("messages", []);
        currentExchangeTooLarge = true;
        return;
      }
      entries = entries.slice(start);
    }
    save("messages", entries);
  }
  retainRecent();
  const publish = () => {
    retainRecent();
    for (const notify of subscribers) notify();
  };
  const session: AssistantSession = {
    get messages() { return entries; },
    get busy() { return Boolean(pending); },
    get persistenceWarning() {
      if (warning) return warning;
      if (currentExchangeTooLarge) return "This exchange is too large to restore after a reload.";
      return trimmed ? "This tab keeps only the most recent exchanges." : "";
    },
    subscribe(changed) { subscribers.add(changed); return () => { subscribers.delete(changed); }; },
    subscribeDraft(changed) {
      draftSubscribers.add(changed);
      changed(dock.draft);
      return () => { draftSubscribers.delete(changed); };
    },
    readDock() { return { ...dock }; },
    updateDock(patch) {
      const draftChanged = patch.draft !== undefined && patch.draft !== dock.draft;
      dock = { ...dock, ...patch };
      if (dock.draft.length > policy.messageBytes || encoder.encode(dock.draft).length > policy.messageBytes) {
        save("dock", { ...dock, draft: "" });
        warning = "Shorten this draft to keep it after a page reload.";
      } else {
        save("dock", dock);
        if (warning === "Shorten this draft to keep it after a page reload.") warning = "";
      }
      if (draftChanged) for (const notify of draftSubscribers) notify(dock.draft);
    },
    async send(value, providerText) {
      const prompt = value.trim();
      if (!prompt || pending || ended || !token || !Number.isSafeInteger(company) || company <= 0) return;
      pending = new AbortController();
      entries.push({ role: "user", text: prompt, state: "ready" }, { role: "assistant", text: "Waiting for reply…", state: "pending" });
      publish();
      let reply: AssistantMessage;
      try {
        const response = await (options.request ?? globalThis.fetch)("/erp/ai/assistant/turn", {
          method: "POST", credentials: "same-origin", redirect: "error", cache: "no-store",
          headers: { "Content-Type": "application/json", "X-CSRF-Token": token },
          body: JSON.stringify({ message: providerText ?? prompt, company_id: company }), signal: pending.signal,
        });
        const data: unknown = response.headers.get("content-type")?.includes("application/json") ? await response.json() : null;
        if (!response.ok) {
          const reason = record(data) && typeof data["message"] === "string" ? data["message"].replace(/^erp\.ai\.[a-z_]+:\s*/, "").trim() : "";
          throw new Error(reason ? reason[0]!.toUpperCase() + reason.slice(1) : "The assistant could not return a reply.");
        }
        if (!record(data) || typeof data["text"] !== "string" || !data["text"].trim()) throw new Error("The provider returned an empty or invalid reply.");
        const canvas = record(data) && data["canvas"] !== undefined ? data["canvas"] : undefined;
        reply = { role: "assistant", text: data["text"], state: "ready", canvas };
      } catch (error) {
        reply = { role: "assistant", state: "error", text: error instanceof TypeError ? "Connection lost. The provider may already have processed this question."
          : error instanceof Error ? error.message : "The assistant could not return a reply." };
      } finally { pending = undefined; }
      if (!ended) { entries[entries.length - 1] = reply; publish(); }
    },
  };
  cache.set(id, session);
  window.addEventListener("erp:session-ended", () => {
    ended = true; pending?.abort(); pending = undefined; entries = []; dock = emptyDock();
    cache.delete(id);
    for (const notify of draftSubscribers) notify("");
    draftSubscribers.clear();
    for (const notify of subscribers) notify(); subscribers.clear();
  }, { once: true });
  return session;
}
