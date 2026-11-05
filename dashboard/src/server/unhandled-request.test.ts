import { DrizzleQueryError } from "drizzle-orm";
import { formatWithOptions } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
import { startInstance } from "#/start";
import { unhandledRequestMiddleware, unhandledRequestResponse } from "#/server/unhandled-request";

const FAKE_SESSION_TOKEN = "fake-session-token-0123456789abc";
const FAKE_LINK_TOKEN = "fake-link-token-0123456789abc";
const FAKE_INVITE_TOKEN = "fake-invite-token-0123456789abc";
const REQUEST_ID = "00000000-0000-4000-8000-000000002877";
const BROWSER = "text/html,application/xhtml+xml,*/*;q=0.8";

afterEach(() => {
  vi.restoreAllMocks();
});

/** Every line the process logs while the test runs. */
function logged() {
  const lines: string[] = [];
  const record = (...args: unknown[]) => {
    lines.push(formatWithOptions({ depth: Infinity }, ...args));
  };
  for (const method of ["log", "info", "warn", "error", "debug", "trace"] as const) {
    vi.spyOn(console, method).mockImplementation(record);
  }
  for (const stream of [process.stdout, process.stderr]) {
    vi.spyOn(stream, "write").mockImplementation((chunk: string | Uint8Array) => {
      lines.push(Buffer.from(chunk).toString("utf8"));
      return true;
    });
  }
  return lines;
}

const failedQuery = () => new DrizzleQueryError('select "id" from "session" where "token" = $1', [FAKE_SESSION_TOKEN]);

it("answers a page with a static page that shows its request id, and logs that id with the path once", async () => {
  const lines = logged();
  const request = new Request(`https://cloud.example.test/pricing?token=${FAKE_LINK_TOKEN}`, { headers: { accept: BROWSER } });

  const response = unhandledRequestResponse(request, failedQuery(), true, REQUEST_ID);

  expect(response.status).toBe(500);
  expect(response.headers.get("content-type")).toBe("text/html; charset=utf-8");
  expect(response.headers.get("x-request-id")).toBe(REQUEST_ID);
  expect(response.headers.get("cache-control")).toBe("no-store");
  const page = await response.text();
  expect(page).toContain("Something went wrong");
  expect(page).toContain(`<code>${REQUEST_ID}</code>`);
  await vi.waitFor(() => expect(lines.join("\n")).toContain("Unhandled request error."));
  const log = lines.join("\n");
  expect(log.split("Unhandled request error.")).toHaveLength(2);
  expect(log).toContain(REQUEST_ID);
  expect(log).toContain("/pricing");
  expect(log).not.toContain(FAKE_LINK_TOKEN);
  expect(log).not.toContain(FAKE_SESSION_TOKEN);
});

it("leaves the path of a page whose URL is a secret out of the log", async () => {
  const lines = logged();
  const request = new Request(`https://cloud.example.test/invite/${FAKE_INVITE_TOKEN}`, { headers: { accept: BROWSER } });

  unhandledRequestResponse(request, new Error("boom"), true, REQUEST_ID);

  await vi.waitFor(() => expect(lines.join("\n")).toContain(REQUEST_ID));
  expect(lines.join("\n")).toContain("[secret page]");
  expect(lines.join("\n")).not.toContain(FAKE_INVITE_TOKEN);
});

it("answers an API route, a server function and a client that asks for JSON with the Public Error and its request id", async () => {
  logged();
  const requests = [
    [new Request("https://cloud.example.test/api/cli/whoami", { headers: { accept: BROWSER } }), true],
    [new Request("https://cloud.example.test/_serverFn/abc", { method: "POST", headers: { accept: BROWSER } }), false],
    [new Request("https://cloud.example.test/", { headers: { accept: "application/json" } }), true],
    [new Request("https://cloud.example.test/", { headers: { accept: "application/json, */*;q=0.1" } }), true],
    [new Request("https://cloud.example.test/", { headers: { accept: "application/json;q=1, text/html;q=0.1" } }), true],
  ] as const;

  for (const [request, page] of requests) {
    const response = unhandledRequestResponse(request, new Error("boom"), page, REQUEST_ID);
    expect(response.status).toBe(500);
    expect(response.headers.get("x-request-id")).toBe(REQUEST_ID);
    expect(await response.json()).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
      requestId: REQUEST_ID,
    });
  }
});

it("turns a throw from a later middleware or handler into that answer, but not an aborted request's", async () => {
  logged();
  const run = unhandledRequestMiddleware.options.server;
  if (run === undefined) throw new Error("the middleware has no server function");
  const call = (cause: unknown, request = new Request("https://cloud.example.test/", { headers: { accept: BROWSER } })) =>
    Promise.resolve(run({ request, handlerType: "router", pathname: "/", context: undefined, next: () => Promise.reject(cause) }));

  const answered: unknown = await call(failedQuery());
  expect(answered).toBeInstanceOf(Response);
  expect(answered instanceof Response && answered.status).toBe(500);

  const left = new AbortController();
  left.abort();
  const gone = new Error("gone");
  await expect(call(gone, new Request("https://cloud.example.test/", { signal: left.signal }))).rejects.toBe(gone);
});

it("runs first, so a throw from every later request middleware and route handler reaches it", async () => {
  expect((await startInstance.getOptions()).requestMiddleware?.[0]).toBe(unhandledRequestMiddleware);
});
