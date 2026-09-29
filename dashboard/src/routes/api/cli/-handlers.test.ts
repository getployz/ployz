import { Effect, Layer } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { MintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { Auth, type AuthService } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { publicErrorResponse, Unauthorized } from "#/server/public-error";

const signedIn = "00000000-0000-4000-8000-000000000001";

function call(body: Record<string, string | boolean>, options: { nodeEnv?: string; bearer?: boolean } = {}) {
  const config = Layer.succeed(AppConfig, asTestDouble<AppConfig["Service"]>()({ nodeEnv: options.nodeEnv ?? "test" }));
  const auth = Layer.succeed(Auth, asTestDouble<AuthService>()({
    resolveActor: () => options.bearer === false ? Effect.fail(new Unauthorized()) : Effect.succeed({ userId: signedIn }),
  }));
  const request = new Request("http://cloud.test/api/cli/servers/enroll", {
    method: "POST",
    body: JSON.stringify(body),
  });
  return Effect.runPromise(
    handleCliRequest(request, MintMachineEnrollmentInput, (actor, input) =>
      Effect.succeed({ actor: actor.userId, organization: input.organizationSlug }),
    ).pipe(Effect.provide(Layer.merge(config, auth))),
  ).catch(publicErrorResponse);
}

describe("signed-in CLI requests", () => {
  it("run the operation as the bearer's Actor with the decoded body", async () => {
    const response = await call({ organizationSlug: "acme" });
    expect(response.status).toBe(200);
    expect(await response.json()).toEqual({ actor: signedIn, organization: "acme" });
  });

  it("refuse a missing sign-in and an invalid body", async () => {
    expect((await call({ organizationSlug: "acme" }, { bearer: false })).status).toBe(401);
    expect((await call({ organizationSlug: "acme", extra: true })).status).toBe(422);
  });

  it("stay dark in production", async () => {
    expect((await call({ organizationSlug: "acme" }, { nodeEnv: "production" })).status).toBe(404);
  });
});
