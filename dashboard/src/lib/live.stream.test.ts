// @vitest-environment jsdom
import { afterEach, expect, it, vi } from "vitest";
import { liveStream, type StreamLoss } from "./live.stream";

const sources: FakeEventSource[] = [];
class FakeEventSource extends EventTarget {
  static CLOSED = 2;
  readyState = 1;
  closed = false;
  constructor(readonly url: string) { super(); sources.push(this); }
  close() { this.closed = true; this.readyState = 2; }
  emit(type: string, data = "{}") { this.dispatchEvent(new MessageEvent(type, { data })); }
}

afterEach(() => { sources.length = 0; vi.useRealTimers(); vi.unstubAllGlobals(); });

it("stays on one connection while the server pings, and starts a fresh one after a silent minute", () => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", FakeEventSource);
  const lost: StreamLoss[] = [];
  const got: string[] = [];
  const close = liveStream("/events", { on: { changes: (event) => got.push(event.data) }, onLost: (why) => lost.push(why) });
  sources[0]?.emit("changes", "a");
  vi.advanceTimersByTime(50_000); sources[0]?.emit("ping"); vi.advanceTimersByTime(50_000);
  expect(sources).toHaveLength(1);
  // 50s since the ping; the next check past a minute reconnects.
  vi.advanceTimersByTime(15_000);
  expect(lost).toEqual(["silent"]);
  expect(sources).toHaveLength(2);
  expect(sources[0]?.closed).toBe(true);
  sources[1]?.emit("changes", "b");
  expect(got).toEqual(["a", "b"]);
  close();
  expect(sources[1]?.closed).toBe(true);
});

it("retries a refused stream with backoff, and says offline once", () => {
  vi.useFakeTimers();
  vi.stubGlobal("EventSource", FakeEventSource);
  const lost: StreamLoss[] = [];
  const close = liveStream("/events", { on: {}, onLost: (why) => lost.push(why) });
  const refuse = (source: FakeEventSource | undefined) => { if (source) { source.readyState = 2; source.dispatchEvent(new Event("error")); } };
  refuse(sources[0]);
  vi.advanceTimersByTime(999);
  expect(sources).toHaveLength(1);
  vi.advanceTimersByTime(1);
  expect(sources).toHaveLength(2);
  refuse(sources[1]);
  vi.advanceTimersByTime(1_999);
  expect(sources).toHaveLength(2);
  vi.advanceTimersByTime(1);
  expect(sources).toHaveLength(3);
  expect(lost).toEqual(["refused"]);
  sources[2]?.dispatchEvent(new Event("open"));
  window.dispatchEvent(new Event("offline"));
  window.dispatchEvent(new Event("offline"));
  expect(lost).toEqual(["refused", "offline"]);
  close();
});
