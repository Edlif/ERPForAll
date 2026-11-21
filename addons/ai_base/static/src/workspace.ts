/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { icon, type ClientActionEnvironment, type ClientActionHandle } from "@erpforall/client";
import { assistantSession, readHistoryPolicy, type AssistantMessage } from "./assistant-session.js";
import { renderReply, workspacePrompt } from "./presentation.js";
import { recordProposal, workspaceRecords } from "./workspace-records.js";

export async function workspace(env: ClientActionEnvironment): Promise<ClientActionHandle> {
  const { currentCompanyId: company, csrfToken: token, userId } = env;
  if (!company || company !== env.action.params["company_id"] || !token || !userId) {
    throw new Error("Reopen AI Assistant in your current company.");
  }
  const chat = assistantSession(env.rpc, company, token, { userId, history: readHistoryPolicy(env.action.params["history"]) });
  const shell = document.createElement("div");
  shell.className = "ai-workspace-shell";
  const main = document.createElement("section");
  main.className = "ai-workspace-conversation";
  const heading = document.createElement("h2");
  heading.textContent = "Conversation";
  const transcript = document.createElement("div");
  transcript.className = "ai-workspace-thread";
  transcript.setAttribute("role", "log");
  transcript.setAttribute("aria-label", "Assistant conversation");
  const hint = document.createElement("p");
  hint.className = "ai-workspace-hint";
  hint.textContent = "Each question is sent separately to your company’s AI provider.";
  const form = document.createElement("form");
  const input = document.createElement("textarea");
  input.rows = 3; input.placeholder = "Ask a question…"; input.setAttribute("aria-label", "Question");
  const send = document.createElement("button");
  send.type = "submit"; send.className = "erp-button erp-button-primary";
  send.append(icon("send"), document.createTextNode("Send"));
  const formatLabel = document.createElement("label");
  const structured = document.createElement("input");
  structured.type = "checkbox"; structured.checked = true;
  formatLabel.append(structured, document.createTextNode("Request a structured reply"));
  const warning = document.createElement("p");
  warning.className = "ai-workspace-hint";
  warning.setAttribute("role", "status");
  const update = () => { send.disabled = chat.busy || !input.value.trim(); };
  const lookup = recordProposal(env, company);
  const rendered = new Map<AssistantMessage, HTMLElement>();
  const empty = document.createElement("p");
  empty.textContent = "What would you like to work on?";
  function render() {
    const nearEnd = transcript.scrollHeight - transcript.clientHeight - transcript.scrollTop < 8;
    warning.textContent = chat.persistenceWarning;
    warning.hidden = !warning.textContent;
    const messages = new Set(chat.messages);
    for (const [message, part] of rendered) {
      if (!messages.has(message)) { part.remove(); rendered.delete(message); }
    }
    if (messages.size) empty.remove();
    else if (!empty.isConnected) transcript.append(empty);
    for (const message of messages) {
      if (rendered.has(message)) continue;
      const part = document.createElement("article");
      part.className = "ai-workspace-message";
      part.dataset["role"] = message.role;
      const author = document.createElement("strong");
      author.textContent = message.role === "user" ? "You" : "AI Assistant";
      const body = document.createElement("div");
      body.className = message.state === "error" ? "ai-workspace-error" : "ai-workspace-text";
      if (message.role === "assistant" && message.state === "ready") renderReply(body, message.text, env, lookup);
      else body.textContent = message.text;
      part.append(author, body);
      transcript.append(part);
      rendered.set(message, part);
    }
    if (nearEnd) transcript.scrollTop = transcript.scrollHeight;
    update();
  }
  form.onsubmit = event => {
    event.preventDefault();
    const message = input.value.trim();
    if (!message || chat.busy) return;
    chat.updateDock({ draft: "" });
    env.run(() => chat.send(message, structured.checked ? workspacePrompt(message) : undefined));
  };
  input.oninput = () => { chat.updateDock({ draft: input.value }); update(); };
  input.onkeydown = event => {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing) {
      event.preventDefault(); if (!send.disabled) form.requestSubmit();
    }
  };
  const records = workspaceRecords(env, company);
  form.append(input, formatLabel, send); main.append(heading, transcript, warning, hint, form);
  shell.append(main, records.node); env.host.append(shell);
  render();
  const unsubscribe = chat.subscribe(render);
  const unsubscribeDraft = chat.subscribeDraft(text => {
    if (input.value !== text) input.value = text;
    warning.textContent = chat.persistenceWarning;
    warning.hidden = !warning.textContent;
    update();
  });
  return { dispose() { unsubscribe(); unsubscribeDraft(); records.dispose(); shell.remove(); } };
}
