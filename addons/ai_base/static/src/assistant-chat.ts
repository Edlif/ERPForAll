/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { icon, type Command, type IconName, type SystrayEnvironment } from "@erpforall/client";

import { renderReply } from "./presentation.js";
import { assistantSession, readHistoryPolicy, type AssistantSession, type AssistantMessage } from "./assistant-session.js";

function control(label: string, glyph: IconName, style: string): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = style;
  button.setAttribute("aria-label", label);
  button.title = label;
  button.append(icon(glyph));
  return button;
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

/** The command palette and toolbar open the same active company window. */
export function assistantCommands({ searchValue }: { searchValue: string }): Command[] {
  const button = document.querySelector<HTMLButtonElement>(".ai-chat-trigger:not([hidden])");
  const name = "Open AI Assistant";
  if (!button || !name.toLowerCase().includes(searchValue.trim().toLowerCase())) return [];
  return [{ id: "ai_base.open_assistant", name, action() {
    if (!button.isConnected || button.hidden) return;
    if (button.getAttribute("aria-expanded") === "true") {
      document.getElementById("ai-assistant-dock")?.querySelector("textarea")?.focus();
    } else {
      button.click();
    }
  } }];
}

/** Authorized dock for the active company, sharing turns with the full-page workspace. */
export function assistantChat(env: SystrayEnvironment, request: typeof fetch = (url, options) => globalThis.fetch(url, options)) {
  const companies = env.context?.["allowed_company_ids"];
  const company: unknown = Array.isArray(companies) ? companies[0] : undefined;
  const token = env.csrfToken;
  const trigger = control("AI Assistant", "message-square", "icon-button ai-chat-trigger");
  trigger.hidden = true;
  trigger.setAttribute("aria-expanded", "false");
  const lifetime = new AbortController();
  let session: AssistantSession | undefined;
  let unsubscribe: (() => void) | undefined;
  let unsubscribeDraft: (() => void) | undefined;
  const dock = document.createElement("section");
  dock.className = "ai-chat-dock";
  dock.id = "ai-assistant-dock";
  trigger.setAttribute("aria-controls", dock.id);
  dock.setAttribute("role", "region");
  dock.setAttribute("aria-label", "AI Assistant");
  dock.hidden = true;
  const header = document.createElement("header");
  const title = document.createElement("h2");
  title.textContent = "AI Assistant";
  const fold = control("Minimize chat", "chevron-down", "icon-button");
  const close = control("Close chat", "x", "icon-button");
  header.append(icon("message-square"), title, fold, close);

  const thread = document.createElement("div");
  thread.className = "ai-chat-thread";
  thread.setAttribute("role", "log");
  thread.setAttribute("aria-label", "Messages");
  thread.setAttribute("aria-live", "polite");
  const form = document.createElement("form");
  const input = document.createElement("textarea");
  input.rows = 2;
  input.placeholder = "Ask a question…";
  input.setAttribute("aria-label", "Message to AI Assistant");
  const hint = document.createElement("p");
  hint.className = "ai-chat-hint";
  hint.textContent = "Each question is sent separately to your company’s AI provider.";
  const send = control("Send message", "send", "erp-button erp-button-primary");
  send.type = "submit";
  send.disabled = true;
  const compose = document.createElement("div");
  compose.className = "ai-chat-compose";
  compose.append(input, send);
  form.append(hint, compose);
  dock.append(header, thread, form);

  const ready = async () => {
    if (typeof company !== "number" || !Number.isSafeInteger(company) || company <= 0 || !token) return;
    const response = await request("/erp/ai/assistant/status", {
      credentials: "same-origin", redirect: "error", cache: "no-store", signal: lifetime.signal,
    });
    if ([401, 403, 404].includes(response.status)) return;
    if (!response.ok) throw new Error("Unable to check whether AI Assistant is available.");
    if (!response.headers.get("content-type")?.includes("application/json")) return;
    const data: unknown = await response.json();
    if (!object(data) || typeof data["available"] !== "boolean") throw new Error("Invalid assistant availability response.");
    if (lifetime.signal.aborted || !data["available"]) return;
    session = assistantSession(env.rpc ?? request, company, token, { ...(env.userId !== undefined && { userId: env.userId }), request, history: readHistoryPolicy(data["history"]) });
    trigger.hidden = false;
    unsubscribeDraft = session.subscribeDraft(text => {
      if (input.value !== text) input.value = text;
      hint.textContent = session?.persistenceWarning || "Each question is sent separately to your company’s AI provider.";
      update();
    });
    unsubscribe = session.subscribe(renderMessages);
    renderMessages();
    const saved = session.readDock();
    if (saved.mode !== "closed") {
      expand(false);
      if (saved.mode === "minimized") minimize(false);
      thread.scrollTop = saved.scroll;
    }
  };
  void ready().catch(() => {
    if (!lifetime.signal.aborted) {
      env.messages?.notifications.add("AI Assistant availability could not be checked.", { level: "warning" });
    }
  });

  const expand = (focus = true) => {
    if (!dock.isConnected) document.body.append(dock);
    dock.hidden = false;
    thread.hidden = form.hidden = false;
    fold.setAttribute("aria-label", "Minimize chat");
    fold.title = "Minimize chat";
    fold.setAttribute("aria-expanded", "true");
    trigger.setAttribute("aria-expanded", "true");
    if (focus) { input.focus(); session?.updateDock({ mode: "expanded" }); }
  };
  const minimize = (focus = true) => {
    thread.hidden = form.hidden = true;
    fold.setAttribute("aria-label", "Expand chat");
    fold.title = "Expand chat";
    fold.setAttribute("aria-expanded", "false");
    trigger.setAttribute("aria-expanded", "false");
    if (focus) { fold.focus(); session?.updateDock({ mode: "minimized" }); }
  };
  trigger.onclick = () => { if (dock.hidden || form.hidden) expand(); else minimize(); };
  fold.onclick = () => { if (form.hidden) expand(); else minimize(); };
  close.onclick = () => { session?.updateDock({ mode: "closed" }); dock.hidden = true; trigger.setAttribute("aria-expanded", "false"); trigger.focus(); };
  dock.addEventListener("keydown", event => {
    if (event.key === "Escape") { event.preventDefault(); event.stopPropagation(); close.click(); }
  });
  const update = () => { send.disabled = !session || session.busy || !input.value.trim(); };
  input.addEventListener("input", () => { session?.updateDock({ draft: input.value }); update(); });
  thread.addEventListener("scroll", () => {
    if (!dock.hidden && !thread.hidden) session?.updateDock({ scroll: Math.max(0, thread.scrollTop) });
  });
  input.addEventListener("keydown", event => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      if (!send.disabled) form.requestSubmit();
    }
  });
  const rendered = new Map<AssistantMessage, HTMLElement>();
  function renderMessages() {
    if (!session) return;
    const atBottom = thread.scrollTop + thread.clientHeight >= thread.scrollHeight - 1;
    const visible = new Set(session.messages);
    for (const [item, part] of rendered) {
      if (!visible.has(item)) { part.remove(); rendered.delete(item); }
    }
    for (const item of visible) {
      if (rendered.has(item)) continue;
      const part = document.createElement("div");
      const author = document.createElement("strong");
      author.textContent = item.role === "user" ? "You" : "AI Assistant";
      part.className = "ai-chat-message " + (item.role === "user" ? "ai-chat-question" : "ai-chat-answer");
      const content = document.createElement("div");
      content.className = "ai-chat-content";
      content.classList.toggle("ai-chat-error", item.state === "error");
      if (item.role === "assistant" && item.state === "ready") renderReply(content, item.text);
      else content.textContent = item.text;
      part.append(author, content);
      thread.append(part);
      rendered.set(item, part);
    }
    hint.textContent = session.persistenceWarning || "Each question is sent separately to your company’s AI provider.";
    if (atBottom) thread.scrollTop = thread.scrollHeight;
    update();
  }
  form.addEventListener("submit", event => {
    event.preventDefault();
    const prompt = input.value.trim();
    if (!session || session.busy || !prompt) return;
    input.value = "";
    session.updateDock({ draft: "" });
    void session.send(prompt);
    thread.scrollTop = thread.scrollHeight;
  });
  return { node: trigger, dispose() { lifetime.abort(); unsubscribe?.(); unsubscribeDraft?.(); dock.remove(); trigger.remove(); } };
}
