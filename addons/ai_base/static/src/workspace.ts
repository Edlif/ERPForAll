/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { type ClientActionEnvironment, type ClientActionHandle } from "@erpforall/client";
import { assistantSession, readHistoryPolicy, type AssistantMessage } from "./assistant-session.js";
import { createCanvasStage } from "./canvas/canvas-stage.js";
import { renderReply } from "./presentation.js";
import { recordProposal, workspaceRecords } from "./workspace-records.js";

export async function workspace(env: ClientActionEnvironment): Promise<ClientActionHandle> {
  const { currentCompanyId: company, csrfToken: token, userId } = env;
  if (!company || company !== env.action.params["company_id"] || !token || !userId) {
    throw new Error("Reopen AI Assistant in your current company.");
  }
  const chat = assistantSession(env.rpc, company, token, {
    userId,
    history: readHistoryPolicy(env.action.params["history"]),
  });

  // Ensure bottom dock is open and readily available for the user
  chat.updateDock({ mode: "expanded" });

  const shell = document.createElement("div");
  shell.className = "ai-workspace-shell ai-workspace-fullpage";

  const lookup = recordProposal(env, company);

  const canvasStage = createCanvasStage(prompt => {
    chat.updateDock({ draft: prompt, mode: "expanded" });
    const dockEl = document.getElementById("ai-assistant-dock");
    if (dockEl) {
      dockEl.hidden = false;
      dockEl.querySelector("textarea")?.focus();
    }
    env.run(() => chat.send(prompt));
  });

  const records = workspaceRecords(env, company);

  // Accessible off-screen transcript kept for screen readers and automated test inspection
  const hiddenTranscript = document.createElement("div");
  hiddenTranscript.className = "ai-workspace-hidden-transcript";
  hiddenTranscript.setAttribute("role", "log");
  hiddenTranscript.setAttribute("aria-live", "polite");
  hiddenTranscript.setAttribute("aria-label", "Assistant conversation history");
  hiddenTranscript.style.display = "none";

  const rendered = new Map<AssistantMessage, HTMLElement>();

  function render() {
    const messages = new Set(chat.messages);
    for (const [message, part] of rendered) {
      if (!messages.has(message)) {
        part.remove();
        rendered.delete(message);
      }
    }

    for (const message of messages) {
      if (rendered.has(message)) continue;
      const part = document.createElement("article");
      part.className = "ai-workspace-message";
      part.dataset["role"] = message.role;

      const body = document.createElement("div");
      body.className = message.state === "error" ? "ai-workspace-error" : "ai-workspace-text";

      if (message.role === "assistant" && message.state === "ready") {
        try {
          if (message.canvas) {
            canvasStage.renderArtifact(message.canvas);
          } else {
            canvasStage.detectAndRender(message.text);
          }
        } catch (canvasErr) {
          console.warn("Canvas stage rendering caught an error:", canvasErr);
        }
        renderReply(body, message.text, env, lookup);
      } else {
        body.textContent = message.text;
      }

      part.append(body);
      hiddenTranscript.append(part);
      rendered.set(message, part);
    }
  }

  shell.append(canvasStage.node, hiddenTranscript);
  env.host.append(shell);
  render();

  const unsubscribe = chat.subscribe(render);

  return {
    dispose() {
      unsubscribe();
      canvasStage.dispose();
      records.dispose();
      shell.remove();
    },
  };
}
