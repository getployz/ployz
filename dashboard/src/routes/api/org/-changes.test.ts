import { setTimeout as sleep } from "node:timers/promises";
import { afterEach, expect, it, vi } from "vitest";
import { handleOrgChangesRequest, type OrgChangesHandlerDeps } from "#/routes/api/org/-changes.handler";
import { NotFound, Unauthorized } from "#/server/public-error";

afterEach(() => {
  vi.useRealTimers();
});

function request(headers: Record<string, string> = {}) {
  return new Request("http://localhost/api/org/changes?organizationSlug=acme", { headers });
}

/** Reads a change stream as text. The stream is pulled, so it only polls while a test reads. */
function readEvents(response: Response) {
  if (!response.body) throw new Error("The change stream has no body");
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  const events = {
    text: "",
    /** Reads until `fragment` arrives or the stream ends. */
    async until(fragment: string) {
      while (!events.text.includes(fragment)) {
        const chunk = await reader.read();
        if (chunk.done) break;
        events.text += decoder.decode(chunk.value);
      }
      return events.text;
    },
    cancel: () => reader.cancel(),
  };
  return events;
}

it.each([
  ["anonymous", new Unauthorized(), 401],
  ["non-member", new NotFound({ message: "The organization was not found." }), 404],
])("refuses %s requests before reading the change log", async (_name, error, status) => {
  const deps = { authorize: vi.fn().mockRejectedValue(error), currentCursor: vi.fn(), readChanges: vi.fn() };
  const response = await handleOrgChangesRequest(request(), "acme", deps);
  expect(response.status).toBe(status);
  expect(deps.readChanges).not.toHaveBeenCalled();
});

it("starts at the current horizon even with a Last-Event-ID, names changed collections, pings, and disables proxy buffering", async () => {
  // Polls wait on real 250 ms timers (node:timers/promises); only the heartbeat timer is faked.
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const readChanges = vi.fn<OrgChangesHandlerDeps["readChanges"]>()
    .mockResolvedValueOnce({ cursor: "50", collections: ["service"] })
    .mockResolvedValue({ cursor: "51", collections: [] });
  const authorize = vi.fn().mockResolvedValue({ organizationId: "org-1" });
  const currentCursor = vi.fn().mockResolvedValue("42");
  const response = await handleOrgChangesRequest(request({ "Last-Event-ID": "7" }), "acme", { authorize, currentCursor, readChanges });

  expect(response.headers.get("Content-Type")).toBe("text/event-stream");
  expect(response.headers.get("X-Accel-Buffering")).toBe("no");
  expect(response.headers.get("Cache-Control")).toBe("private, no-store, no-transform");
  const events = readEvents(response);

  await events.until("event: changes");
  expect(readChanges).toHaveBeenNthCalledWith(1, { organizationId: "org-1", since: "42" });
  expect(events.text).toContain('event: changes\ndata: {"collections":["service"]}\n\n');
  expect(events.text).not.toContain("id: ");
  await vi.waitFor(() => expect(readChanges).toHaveBeenNthCalledWith(2, { organizationId: "org-1", since: "50" }));
  expect(events.text).not.toContain(": ping");
  await vi.advanceTimersByTimeAsync(15_000);
  await events.until(": ping\n\n");
  // Quiet polls advance the cursor without emitting events.
  expect(events.text.match(/event: changes/g)).toHaveLength(1);
  await vi.waitFor(() => expect(readChanges).toHaveBeenLastCalledWith({ organizationId: "org-1", since: "51" }));
  await events.cancel();
  const calls = readChanges.mock.calls.length;
  await sleep(600);
  expect(readChanges).toHaveBeenCalledTimes(calls);
});

it("ends the stream when the change log can't be read, so EventSource reconnects", async () => {
  const readChanges = vi.fn<OrgChangesHandlerDeps["readChanges"]>().mockRejectedValue(new Error("database down"));
  const response = await handleOrgChangesRequest(request(), "acme", {
    authorize: vi.fn().mockResolvedValue({ organizationId: "org-1" }),
    currentCursor: vi.fn().mockResolvedValue("6"),
    readChanges,
  });
  // The stream ends instead of reaching a changes event.
  expect(await readEvents(response).until("event: changes")).toBe("retry: 1000\n\n");
  expect(readChanges).toHaveBeenCalledOnce();
});

it("opens and immediately ends the stream when the current horizon can't be read, so EventSource retries", async () => {
  const readChanges = vi.fn<OrgChangesHandlerDeps["readChanges"]>();
  const response = await handleOrgChangesRequest(request(), "acme", {
    authorize: vi.fn().mockResolvedValue({ organizationId: "org-1" }),
    currentCursor: vi.fn().mockRejectedValue(new Error("database down")),
    readChanges,
  });
  expect(response.status).toBe(200);
  expect(response.headers.get("Content-Type")).toBe("text/event-stream");
  expect(await response.text()).toBe("retry: 1000\n\n");
  expect(readChanges).not.toHaveBeenCalled();
});

it("takes the starting horizon before the response opens, so the refetch on open can't miss a change", async () => {
  let taken = false;
  const response = await handleOrgChangesRequest(request(), "acme", {
    authorize: vi.fn().mockResolvedValue({ organizationId: "org-1" }),
    currentCursor: async () => {
      await Promise.resolve();
      taken = true;
      return "6";
    },
    readChanges: vi.fn<OrgChangesHandlerDeps["readChanges"]>().mockResolvedValue({ cursor: "6", collections: [] }),
  });
  expect(taken).toBe(true);
  await response.body?.cancel();
});
