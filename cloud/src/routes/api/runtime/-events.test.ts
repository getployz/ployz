import { beforeEach, describe, expect, it, vi } from "vitest";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";

import { handleRuntimeEventsRequest } from "#/routes/api/runtime/-events.handler";
import { NotFound, Unauthorized } from "#/server/public-error";

const mocks = {
  openRuntimeWatch: vi.fn(),
};

function handle(request: Request, organizationSlug: string) {
  return handleRuntimeEventsRequest(request, organizationSlug, mocks);
}

describe("runtime events route", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it.each([
    [
      "unauthorized",
      new Unauthorized(),
      401,
      "UNAUTHORIZED",
    ],
    [
      "missing organization",
      new NotFound({ message: "Organization not found: acme" }),
      404,
      "NOT_FOUND",
    ],
    [
      "runtime-watch establishment failure",
      new RuntimeConnectionFailure({
        cause: new Error("private server detail"),
      }),
      500,
      "INTERNAL",
    ],
  ])("maps %s before opening a stream", async (_name, error, status, code) => {
    mocks.openRuntimeWatch.mockRejectedValue(error);
    const response = await handle(
      new Request("http://localhost/api/runtime/events?organizationSlug=acme"),
      "acme",
    );

    expect({ status: response.status, body: await response.json() }).toEqual({
      status,
      body: expect.objectContaining({ _tag: "PublicError", code }),
    });
  });

  it("streams a stable no-connection status when no cluster is configured", async () => {
    mocks.openRuntimeWatch.mockResolvedValue({ status: "no_connection" });
    const response = await handle(
      new Request("http://localhost/api/runtime/events?organizationSlug=acme"),
      "acme",
    );
    const reader = response.body?.getReader();
    await reader?.read();
    const status = await reader?.read();

    expect(new TextDecoder().decode(status?.value)).toContain(
      "event: runtime.lens\n",
    );
    await reader?.cancel();
  });

  it("streams a distinct unreachable status when the cluster cannot be reached", async () => {
    mocks.openRuntimeWatch.mockResolvedValue({
      status: "unreachable",
      error: null,
    });
    const response = await handle(
      new Request("http://localhost/api/runtime/events?organizationSlug=acme"),
      "acme",
    );
    const reader = response.body?.getReader();
    await reader?.read();
    const status = await reader?.read();

    expect(new TextDecoder().decode(status?.value)).toContain(
      'event: runtime.lens\ndata: {"status":"unreachable"',
    );
    await reader?.cancel();
  });

  it("streams a projected runtime lens and closes the SDK watch", async () => {
    const close = vi.fn(async () => undefined);
    const watchFrame = runtimeWatchFrameFixture();
    async function* frames() {
      yield watchFrame;
      await new Promise(() => undefined);
    }
    mocks.openRuntimeWatch.mockResolvedValue({
      status: "connected",
      frames: frames(),
      close,
    });
    const response = await handle(
      new Request("http://localhost/api/runtime/events?organizationSlug=acme"),
      "acme",
    );
    const reader = response.body?.getReader();
    await reader?.read();
    const event = await reader?.read();

    expect(new TextDecoder().decode(event?.value)).toContain(
      'event: runtime.lens\ndata: {"status":"live_empty"',
    );
    await reader?.cancel();
    expect(close).toHaveBeenCalledOnce();
  });
});
