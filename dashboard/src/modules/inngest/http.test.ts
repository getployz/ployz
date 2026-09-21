import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { handleInngestHttp } from "#/modules/inngest/http";

describe("Inngest execution origin", () => {
  it("registers private URLs even when startup sync uses loopback and rejects unsigned execution", async () => {
    const registration = vi.fn<typeof fetch>(async () => Response.json({ status: 200, modified: true }));
    const client = new Inngest({
      id: "execution-origin-test",
      isDev: false,
      signingKey: "signkey-test-0123456789abcdef",
      fetch: registration,
    });
    const origin = "http://web.railway.internal:8080";
    const response = await Effect.runPromise(handleInngestHttp(client, origin, new Request("http://127.0.0.1:8080/api/inngest", {
      method: "PUT",
      headers: { "content-type": "application/json", "x-inngest-server-kind": "cloud", host: "127.0.0.1:8080" },
      body: "{}",
    })));
    expect(response.status).toBe(200);
    const body = JSON.parse(String(registration.mock.calls[0]?.[1]?.body));
    expect(body.url).toBe(`${origin}/api/inngest`);
    expect(body.functions.length).toBeGreaterThan(0);
    for (const fn of body.functions) {
      expect(fn.steps.step.runtime.url).toContain(`${origin}/api/inngest?`);
    }

    const unsigned = await Effect.runPromise(handleInngestHttp(client, origin, new Request(`${origin}/api/inngest`, {
      method: "POST", body: "{}", headers: { "content-type": "application/json", host: "web.railway.internal:8080" },
    })));
    expect(unsigned.status).toBe(401);
  });
});
