import { Effect } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import { OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { handleRuntimeEventsRequest } from "#/routes/api/runtime/-events.handler";
import { Auth, type AuthService } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { publicErrorResponse } from "#/server/public-error";
import { testConfigLayer } from "#/test/config-environment";

describe("runtime events route", () => {
  it("answers 401 before opening a runtime when the request has no session", async () => {
    const refused = await Effect.runPromise(
      handleRuntimeEventsRequest(new Request("http://localhost/api/runtime/events?organizationSlug=acme"), "acme").pipe(
        Effect.flip,
        Effect.provideService(Auth, asTestDouble<AuthService>()({ getSession: () => Effect.succeed(null) })),
        Effect.provideService(Database, undefined as never),
        Effect.provideService(InngestClient, new Inngest({ id: "runtime-events-test" })),
        Effect.provideService(
          OrganizationRuntime,
          asTestDouble<OrganizationRuntimeService>()({ open: () => Effect.die("must not open runtime") }),
        ),
        Effect.provide(testConfigLayer({ DATABASE_URL: "postgres://unused" })),
      ),
    );

    expect(publicErrorResponse(refused).status).toBe(401);
  });
});
