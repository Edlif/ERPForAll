/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, expect, test, vi } from "vitest";
import { assistantChat } from "../src/assistant-chat.js";
import { sessionPanel, type SessionPanelHandle } from "../src/session-panel.js";
import type { ResumeSessionResult, SessionSummary } from "../src/session-contract.js";

const open: SessionPanelHandle[] = [];

function panel(
  call: ReturnType<typeof vi.fn>,
  options: { context?: Record<string, unknown>; onResume?: (session: SessionSummary, resume: ResumeSessionResult) => void } = {},
): SessionPanelHandle {
  const handle = sessionPanel({ rpc: { call }, context: options.context, onResume: options.onResume });
  open.push(handle);
  document.body.append(handle.node);
  return handle;
}

afterEach(() => {
  for (const handle of open.splice(0)) handle.dispose();
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function summary(over: Record<string, unknown> = {}): Record<string, unknown> {
  return { id: 1, title: "Weekly revenue", state: "open", last_event_at: "2026-11-21T12:00:00Z", anchor: null, ...over };
}

function rows(): string[] {
  return Array.from(document.querySelectorAll(".ai-session-row"), row => String(row.getAttribute("data-session-id")));
}

function titles(): string[] {
  return Array.from(document.querySelectorAll(".ai-session-title"), node => node.textContent ?? "");
}

/// (partial: the panel renders the server's last-activity order and hands the server sequence to the chat on resume)
test("sessions render in the server's last-activity order and resume hands the sequence to the chat", async () => {
  const resumes: [SessionSummary, ResumeSessionResult][] = [];
  const call = vi.fn()
    .mockResolvedValueOnce([
      summary({ id: 5, title: "Newest", last_event_at: "2026-11-21T12:00:00Z" }),
      summary({ id: 3, title: "Older thread", last_event_at: "2026-11-19T12:00:00Z" }),
    ])
    .mockResolvedValueOnce({ id: 3, last_seq: 12, anchor: null });
  const view = panel(call, { onResume: (session, resume) => resumes.push([session, resume]) });
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(2));
  expect(call).toHaveBeenCalledWith("ai.session", "search_sessions", { search: "", include_archived: false });
  // Without an open record the panel renders the server's order verbatim; the only reordering it
  // ever does is the deliberate hoist of the about group (covered separately).
  expect(rows()).toEqual(["5", "3"]);
  expect(Array.from(document.querySelectorAll(".ai-session-time"), node => node.textContent)).toEqual(["Nov 21", "Nov 19"]);
  view.node.querySelector<HTMLButtonElement>('[data-session-id="3"] .ai-session-title')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
  expect(call).toHaveBeenLastCalledWith("ai.session", "resume_session", { id: 3 });
  await vi.waitFor(() => expect(resumes.length).toBe(1));
  expect(resumes[0]![1]).toEqual({ id: 3, last_seq: 12, anchor: null });
  expect(resumes[0]![0].id).toBe(3);
  const active = view.node.querySelector<HTMLElement>('[data-session-id="3"] .ai-session-title');
  expect(active?.getAttribute("aria-current")).toBe("true");
});

/// (partial: one search term reaches the server, which matches titles and the owner's own message words)
test("one search term covers title and own-message matches", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 1, title: "Supplier check" })])
    .mockResolvedValueOnce([summary({ id: 2, title: "PO-0041 follow-up" })])
    .mockResolvedValueOnce([summary({ id: 4, title: "Quarterly VAT" })]);
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(1));
  const search = view.node.querySelector<HTMLInputElement>(".ai-session-search")!;
  // A word of the owner's own message.
  search.value = "invoice";
  search.dispatchEvent(new Event("input"));
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
  expect(call).toHaveBeenLastCalledWith("ai.session", "search_sessions", { search: "invoice", include_archived: false });
  await vi.waitFor(() => expect(titles()).toEqual(["PO-0041 follow-up"]));
  // A word of a title.
  search.value = "quarterly";
  search.dispatchEvent(new Event("input"));
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(3));
  expect(call).toHaveBeenLastCalledWith("ai.session", "search_sessions", { search: "quarterly", include_archived: false });
  await vi.waitFor(() => expect(titles()).toEqual(["Quarterly VAT"]));
});

/// (partial: archiving removes the session from the open list, the archived list keeps it readable and restore reopens it)
test("archive removes a session from the open list and the archived list keeps it readable", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 1, title: "Open one" }), summary({ id: 2, title: "To archive" })])
    .mockResolvedValueOnce({ id: 2, state: "archived" })
    .mockResolvedValueOnce([summary({ id: 1, title: "Open one" })])
    .mockResolvedValueOnce([summary({ id: 1, title: "Open one" }), summary({ id: 2, title: "To archive", state: "archived" })])
    .mockResolvedValueOnce({ id: 2, state: "open" })
    .mockResolvedValueOnce([summary({ id: 1, title: "Open one" }), summary({ id: 2, title: "To archive" })]);
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(2));
  view.node.querySelector<HTMLButtonElement>('[data-session-id="2"] [aria-label="Archive session"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledWith("ai.session", "archive_session", { id: 2, state: "archived" }));
  // The archived one left the open list.
  await vi.waitFor(() => expect(rows()).toEqual(["1"]));
  // …and stays readable from the archived list.
  view.node.querySelector<HTMLInputElement>(".ai-session-archived-toggle input")!.click();
  await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith("ai.session", "search_sessions", { search: "", include_archived: true }));
  await vi.waitFor(() => expect(rows()).toEqual(["1", "2"]));
  expect(document.querySelector('[data-session-id="2"] .ai-session-badge')?.textContent).toBe("Archived");
  document.querySelector<HTMLButtonElement>('[data-session-id="2"] [aria-label="Restore session"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith("ai.session", "archive_session", { id: 2, state: "open" }));
  await vi.waitFor(() => expect(document.querySelector('[data-session-id="2"] .ai-session-badge')).toBeNull());
});

/// (partial: the panel shows "Anchor not available" and never sends an anchor value; the turn-level withholding is the server slice's)
test("an unreadable anchor shows only the unavailable state and no anchor value travels with resume", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([
      summary({ id: 7, title: "Partner notes", anchor: { model: "res.partner", res_id: 5, available: false } }),
      summary({ id: 8, title: "Readable anchor", anchor: { model: "res.partner", res_id: 9, available: true } }),
    ])
    .mockResolvedValueOnce({ id: 7, last_seq: 3, anchor: { model: "res.partner", res_id: 5, available: false } });
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(2));
  const missing = view.node.querySelector<HTMLElement>('[data-session-id="7"] .ai-session-meta')!;
  expect(missing.textContent).toContain("Anchor not available");
  // No anchor identifier or value is shown for the unreadable anchor.
  expect(missing.textContent).not.toContain("res.partner");
  expect(missing.textContent).not.toContain("#5");
  const readable = view.node.querySelector<HTMLElement>('[data-session-id="8"] .ai-session-meta')!;
  expect(readable.textContent).toContain("About res.partner #9");
  view.node.querySelector<HTMLButtonElement>('[data-session-id="7"] .ai-session-title')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(2));
  // The resume payload is exactly the id: no anchor value reaches any request.
  expect(call).toHaveBeenLastCalledWith("ai.session", "resume_session", { id: 7 });
});

/// (partial: the panel asks the server for the open record's sessions with the `about` filter instead of filtering client-side; the owner scoping itself is the server's)
test("about this record is the server's about-filtered answer, and the general group never repeats it", async () => {
  const call = vi.fn()
    // The about call: only the sessions the server matched for the open record.
    .mockResolvedValueOnce([
      summary({ id: 11, title: "Partner A follow-up", anchor: { model: "res.partner", res_id: 12, available: true } }),
    ])
    // The general call: the same session appears again plus an unrelated one; the panel never repeats it.
    .mockResolvedValueOnce([
      summary({ id: 11, title: "Partner A follow-up", anchor: { model: "res.partner", res_id: 12, available: true } }),
      summary({ id: 12, title: "Order review", anchor: { model: "sale.order", res_id: 99, available: true } }),
    ]);
  const view = panel(call, { context: { active_model: "res.partner", active_id: 12 } });
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(2));
  // The about section is its own server call with the identifier filter, never a client-side scan.
  expect(call).toHaveBeenNthCalledWith(1, "ai.session", "search_sessions", {
    search: "", include_archived: false, about: { model: "res.partner", res_id: 12 },
  });
  expect(call).toHaveBeenNthCalledWith(2, "ai.session", "search_sessions", { search: "", include_archived: false });
  const groups = Array.from(view.node.querySelectorAll(".ai-session-group"));
  const about = groups.find(group => group.querySelector("h3")?.textContent === "About this record")!;
  expect(about.querySelectorAll(".ai-session-row").length).toBe(1);
  expect(about.querySelector(".ai-session-title")?.textContent).toBe("Partner A follow-up");
  const all = groups.find(group => group.querySelector("h3")?.textContent === "Sessions")!;
  expect(all.querySelectorAll(".ai-session-row").length).toBe(1);
  expect(all.querySelector(".ai-session-title")?.textContent).toBe("Order review");
});

test("an empty store shows the empty state and a failure shows the error with a working retry", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([])
    .mockRejectedValueOnce(new Error("Access denied"))
    .mockResolvedValueOnce([summary({ id: 1, title: "Recovered" })]);
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelector(".ai-session-empty")?.textContent).toContain("No sessions yet."));
  await view.refresh();
  await vi.waitFor(() => expect(document.querySelector('[role="alert"]')?.textContent).toContain("Access denied"));
  document.querySelector<HTMLButtonElement>('[role="alert"] button')!.click();
  await vi.waitFor(() => expect(document.querySelector(".ai-session-title")?.textContent).toBe("Recovered"));
});

test("fork, delete with an inline confirmation and export go through the contract", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 1, title: "Source" })])
    .mockResolvedValueOnce({ id: 9 })
    .mockResolvedValueOnce([summary({ id: 1, title: "Source" }), summary({ id: 9, title: "Forked copy" })])
    .mockResolvedValueOnce({ id: 1 })
    .mockResolvedValueOnce([summary({ id: 9, title: "Forked copy" })])
    .mockResolvedValueOnce({ format: "json", filename: "assistant-session-9.json", content: "{}" });
  vi.stubGlobal("URL", Object.assign({}, URL, { createObjectURL: vi.fn(() => "blob:test"), revokeObjectURL: vi.fn() }));
  const downloads: string[] = [];
  vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
    downloads.push(this.download);
  });
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(1));
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Fork session"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledWith("ai.session", "fork_session", { id: 1 }));
  await vi.waitFor(() => expect(rows()).toEqual(["1", "9"]));
  // Delete asks once more, inline, before it removes anything.
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Delete session"]')!.click();
  expect(document.querySelector('[data-session-id="1"] [aria-label="Confirm delete"]')).not.toBeNull();
  expect(call).not.toHaveBeenCalledWith("ai.session", "session_delete", expect.anything());
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Confirm delete"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledWith("ai.session", "session_delete", { id: 1 }));
  await vi.waitFor(() => expect(rows()).toEqual(["9"]));
  // Export turns the server's JSON into a file download.
  document.querySelector<HTMLButtonElement>('[data-session-id="9"] [aria-label="Export session"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenLastCalledWith("ai.session", "session_export", { id: 9 }));
  await vi.waitFor(() => expect(downloads).toEqual(["assistant-session-9.json"]));
});

/// (partial: the client surfaces the retention slice's always-on refusal with user copy and leaves the confirm pose; the deletion path itself is the server slice's)
test("the delete refusal shows friendly copy and the row leaves the confirm pose", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 1, title: "Source" })])
    .mockRejectedValueOnce(new Error(
      "erp.ai.session_delete_pending: deleting a session arrives with AI1.SESSION-RETENTION; the session stays until that slice lands.",
    ));
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(1));
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Delete session"]')!.click();
  expect(document.querySelector('[data-session-id="1"] [aria-label="Confirm delete"]')).not.toBeNull();
  expect(call).not.toHaveBeenCalledWith("ai.session", "session_delete", expect.anything());
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Confirm delete"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledWith("ai.session", "session_delete", { id: 1 }));
  // Friendly copy on the status line, never the code or the ADR jargon.
  await vi.waitFor(() => expect(view.node.querySelector(".ai-session-status")?.textContent).toContain("Deleting sessions isn't available yet."));
  expect(document.body.textContent).not.toContain("erp.ai.session_delete_pending");
  // A regular expression, not the citation text: the public export rewrites a literal "ADR <number>" to an empty string.
  expect(document.body.textContent).not.toMatch(/\bADR\b/);
  // The row left the confirm pose and the session is still listed.
  expect(document.querySelector('[data-session-id="1"] [aria-label="Confirm delete"]')).toBeNull();
  expect(document.querySelector('[data-session-id="1"] [aria-label="Delete session"]')).not.toBeNull();
  expect(document.querySelectorAll(".ai-session-row").length).toBe(1);
});

test("a mid-list action failure shows the friendly error and keeps the list", async () => {
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 1, title: "Source" })])
    .mockRejectedValueOnce(new Error("erp.ai.about_invalid: the about filter is exactly {model: string, res_id: positive integer}."));
  const view = panel(call);
  await vi.waitFor(() => expect(document.querySelectorAll(".ai-session-row").length).toBe(1));
  document.querySelector<HTMLButtonElement>('[data-session-id="1"] [aria-label="Fork session"]')!.click();
  await vi.waitFor(() => expect(call).toHaveBeenCalledWith("ai.session", "fork_session", { id: 1 }));
  // The unknown `erp.ai.*` code never reaches the user; the generic refusal does.
  await vi.waitFor(() => expect(view.node.querySelector(".ai-session-status")?.textContent).toContain("The session store refused the request."));
  expect(document.body.textContent).not.toContain("erp.ai.about_invalid");
  expect(document.querySelectorAll(".ai-session-row").length).toBe(1);
});

test("the about section re-reads the live record identity when the panel is shown again", async () => {
  // The systray context object is live: navigation mutates the same object the panel was given.
  const context: Record<string, unknown> = { active_model: "res.partner", active_id: 12 };
  const call = vi.fn()
    .mockResolvedValueOnce([summary({ id: 11, title: "Partner follow-up", anchor: { model: "res.partner", res_id: 12, available: true } })])
    .mockResolvedValueOnce([summary({ id: 11, title: "Partner follow-up", anchor: { model: "res.partner", res_id: 12, available: true } })])
    .mockResolvedValueOnce([])
    .mockResolvedValueOnce([summary({ id: 20, title: "Order thread", anchor: { model: "sale.order", res_id: 99, available: true } })]);
  const view = panel(call, { context });
  await vi.waitFor(() => expect(view.node.querySelector(".ai-session-title")?.textContent).toBe("Partner follow-up"));
  expect(call).toHaveBeenCalledTimes(2);
  // The record behind the panel changed: the shell updates the same context object.
  context["active_model"] = "sale.order";
  context["active_id"] = 99;
  view.node.hidden = true;
  view.node.hidden = false;
  await vi.waitFor(() => expect(call).toHaveBeenCalledTimes(4));
  expect(call).toHaveBeenNthCalledWith(3, "ai.session", "search_sessions", {
    search: "", include_archived: false, about: { model: "sale.order", res_id: 99 },
  });
  expect(call).toHaveBeenNthCalledWith(4, "ai.session", "search_sessions", { search: "", include_archived: false });
  await vi.waitFor(() => expect(view.node.querySelector(".ai-session-title")?.textContent).toBe("Order thread"));
});

test("the dock header opens the panel and Escape returns to the chat", async () => {
  const call = vi.fn().mockResolvedValue([summary({ id: 1, title: "First session" })]);
  const request = vi.fn<typeof fetch>().mockResolvedValue(new Response(
    JSON.stringify({ available: true, history: { turns: 4, bytes: 4096, message_bytes: 4096 } }),
    { headers: { "content-type": "application/json" } },
  ));
  const env = { csrfToken: "session-test", context: { allowed_company_ids: [7] }, rpc: { call } };
  const chat = assistantChat(env, request);
  document.body.append(chat.node);
  try {
    await vi.waitFor(() => expect(chat.node.hidden).toBe(false));
    chat.node.click();
    const dock = document.getElementById(chat.node.getAttribute("aria-controls")!)!;
    const sessionsButton = dock.querySelector<HTMLButtonElement>(".ai-chat-sessions-trigger")!;
    expect(sessionsButton.hidden).toBe(false);
    sessionsButton.click();
    const panelEl = dock.querySelector<HTMLElement>(".ai-session-panel")!;
    expect(panelEl.hidden).toBe(false);
    expect(sessionsButton.getAttribute("aria-expanded")).toBe("true");
    expect(dock.querySelector(".ai-chat-thread")!.hidden).toBe(true);
    expect(dock.querySelector("form")!.hidden).toBe(true);
    await vi.waitFor(() => expect(document.querySelector(".ai-session-title")?.textContent).toBe("First session"));
    expect(document.activeElement).toBe(panelEl.querySelector(".ai-session-search"));
    panelEl.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(panelEl.hidden).toBe(true);
    expect(sessionsButton.getAttribute("aria-expanded")).toBe("false");
    expect(dock.querySelector(".ai-chat-thread")!.hidden).toBe(false);
    expect(document.activeElement).toBe(dock.querySelector("textarea"));
  } finally {
    chat.dispose();
  }
});

test("the sessions button stays hidden without an RPC client", async () => {
  const request = vi.fn<typeof fetch>().mockResolvedValue(new Response(
    JSON.stringify({ available: true, history: { turns: 4, bytes: 4096, message_bytes: 4096 } }),
    { headers: { "content-type": "application/json" } },
  ));
  const chat = assistantChat({ csrfToken: "session-test", context: { allowed_company_ids: [7] } }, request);
  document.body.append(chat.node);
  try {
    await vi.waitFor(() => expect(chat.node.hidden).toBe(false));
    chat.node.click();
    const dock = document.getElementById(chat.node.getAttribute("aria-controls")!)!;
    expect(dock.querySelector<HTMLButtonElement>(".ai-chat-sessions-trigger")!.hidden).toBe(true);
  } finally {
    chat.dispose();
  }
});
