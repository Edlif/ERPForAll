/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { expect, test, vi } from "vitest";
import { watchChatterThread } from "../src/client/chatter-realtime.js";

test("an authenticated thread resynchronizes on initial subscription and after a reconnect", async () => {
  vi.useFakeTimers();
  try {
    const sockets: EventTarget[] = [];
    const sent: string[] = [];
    const connect = () => {
      const socket = Object.assign(new EventTarget(), { send: (text: string) => sent.push(text), close: vi.fn() });
      sockets.push(socket);
      return socket as unknown as WebSocket;
    };
    const reload = vi.fn(async () => {});
    const stop = watchChatterThread({ model: "purchase.order", id: 23 }, { company: 2, csrf: "fixture", retry: [500, 30000] }, reload, vi.fn(), connect);
    sockets[0]!.dispatchEvent(new Event("open"));
    expect(JSON.parse(sent[0]!)).toEqual({ type: "subscribe", model: "purchase.order", id: 23, company: 2, csrf: "fixture" });
    sockets[0]!.dispatchEvent(new MessageEvent("message", {data:'{"type":"resync"}'}));
    expect(reload).toHaveBeenCalledTimes(1);
    sockets[0]!.dispatchEvent(new CloseEvent("close", {code: 1006}));
    await vi.runOnlyPendingTimersAsync();
    sockets[1]!.dispatchEvent(new Event("open"));
    sockets[1]!.dispatchEvent(new MessageEvent("message", {data:'{"type":"resync"}'}));
    expect(reload).toHaveBeenCalledTimes(2);
    stop();
  } finally { vi.useRealTimers(); }
});
