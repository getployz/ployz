import { Cause, Effect, Exit } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { authorizeRuntimeOrganization } from "#/modules/runtime/authorize-runtime-organization.server";
import {
  Auth,
  AuthenticationUnavailable,
  type AuthService,
} from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { publicErrorResponse, Unauthorized } from "#/server/public-error";

function authorizeWithSession(getSession: AuthService["getSession"]) {
  return authorizeRuntimeOrganization({
    headers: new Headers(),
    organizationSlug: "acme",
  }).pipe(
    Effect.provideService(
      Auth,
      asTestDouble<AuthService>()({ getSession }),
    ),
    Effect.provideService(Database, undefined as never),
    Effect.exit,
  );
}

describe("authorizeRuntimeOrganization", () => {
  it("returns 401 only when the session is missing", async () => {
    const exit = await Effect.runPromise(
      authorizeWithSession(() => Effect.succeed(null)),
    );

    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      const failure = Cause.squash(exit.cause);
      expect(failure).toBeInstanceOf(Unauthorized);
      expect(publicErrorResponse(failure).status).toBe(401);
    }
  });

  it("does not map session infrastructure failures to 401", async () => {
    const exit = await Effect.runPromise(
      authorizeWithSession(() =>
        Effect.fail(
          new AuthenticationUnavailable({
            cause: new Error("connection refused"),
          }),
        ),
      ),
    );

    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      const failure = Cause.squash(exit.cause);
      expect(failure).toBeInstanceOf(AuthenticationUnavailable);
      expect(publicErrorResponse(failure).status).toBe(500);
    }
  });
});
