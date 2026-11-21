/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { afterEach, expect, test, vi } from "vitest";
import { assistantSession } from "../src/assistant-session.js";
import { clearProtectedTabState } from "../../../web/static/src/client/session-boundary.js";

afterEach(() => clearProtectedTabState());
test("a fresh shell restores replies and dock state in the same user/company scope only", async () => {
  const request = vi.fn<typeof fetch>().mockImplementation(async () => Response.json({ text: "Received 6" }));
  const options = { userId: 51, request, history: { turns: 4, bytes: 4096, messageBytes: 4096 } };
  const session = assistantSession({}, 7, "token-1", options);
  session.updateDock({ mode: "minimized", draft: "What remains?", scroll: 86 });
  await session.send("Show receipt", "Format the response as requested. Show receipt");
  expect(session.messages.map(m => m.text)).toEqual(["Show receipt","Received 6"]);
  expect(JSON.parse(String(request.mock.calls[0]![1]?.body)).message).toBe("Format the response as requested. Show receipt");
  const restored = assistantSession({}, 7, "rotated-token", options);
  expect(restored.messages).toEqual(session.messages);
  expect(restored.readDock()).toEqual({ mode: "minimized", draft: "What remains?", scroll: 86 });
  expect(assistantSession({}, 8, "other-company", options).messages).toEqual([]);
  expect(assistantSession({}, 7, "other-user", { ...options, userId: 52 }).readDock().draft).toBe("");
  clearProtectedTabState();
  expect(restored.messages).toEqual([]);
  expect(assistantSession({}, 7, "new-login", options).messages).toEqual([]);
});

test("a reload interrupts a pending exchange without replaying a provider call", async () => {
  let finish!: (value: Response) => void;
  const request = vi.fn<typeof fetch>().mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  const options = { userId: 51, request, history: { turns: 4, bytes: 4096, messageBytes: 4096 } };
  const first = assistantSession({}, 7, "token", options);
  const sending = first.send("Summarize");
  const recovered = assistantSession({}, 7, "rotated-token", options);
  expect(recovered.messages.at(-1)?.state).toBe("error");
  expect(recovered.busy).toBe(false);
  expect(request).toHaveBeenCalledTimes(1);
  clearProtectedTabState();
  finish(Response.json({ text: "A late reply" }));
  await sending;
  expect(assistantSession({}, 7, "new-login", options).messages).toEqual([]);
});

test("tab history evicts whole exchanges and restores only the configured number", async () => {
  const options = { userId: 9, request: vi.fn(async () => Response.json({ text: "Received" })),
    history: { turns: 2, bytes: 4096, messageBytes: 4096 } };
  const session = assistantSession({}, 6, "token", options);
  for (const question of ["One", "Two", "Three"]) await session.send(question);
  expect(session.messages.map(item => item.text)).toEqual(["Two", "Received", "Three", "Received"]);
  const restored = assistantSession({}, 6, "token", options);
  expect(restored.messages).toEqual(session.messages);
  expect(restored.persistenceWarning).toContain("most recent");
});

test("an oversized reply remains readable now and never replays on reload", async () => {
  const text = "詳".repeat(1600);
  const request = vi.fn(async () => Response.json({ text }));
  const options = { userId: 5, request, history: { turns: 2, bytes: 4096, messageBytes: 4096 } };
  const session = assistantSession({}, 7, "token", options);
  await session.send("Show all");
  expect(session.messages.at(-1)?.text).toBe(text);
  expect(session.persistenceWarning).toContain("too large");
  expect(assistantSession({}, 7, "token", options).messages).toEqual([]);
  expect(request).toHaveBeenCalledTimes(1);
  request.mockImplementationOnce(async () => Response.json({ text: "Ready" }));
  await session.send("Short reply");
  expect(session.persistenceWarning).toContain("most recent");
  expect(assistantSession({}, 7, "token", options).messages).toEqual(session.messages);
});

test("editors share draft updates without re-rendering the transcript", () => {
  const options = { userId: 50, history: { turns: 4, bytes: 4096, messageBytes: 80 } };
  const session = assistantSession({}, 8, "token", options);
  const transcript = vi.fn(), editor = vi.fn();
  session.subscribe(transcript);
  const dispose = session.subscribeDraft(editor);
  session.updateDock({ draft: "PO00001" });
  session.updateDock({ scroll: 4 });
  expect(editor.mock.calls).toEqual([[""], ["PO00001"]]);
  expect(transcript).not.toHaveBeenCalled();
  dispose();
  session.updateDock({ draft: "Later" });
  expect(editor).toHaveBeenCalledTimes(2);
});
