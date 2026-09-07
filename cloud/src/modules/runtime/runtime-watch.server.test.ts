import type { RuntimeWatchView } from "@ployz/sdk";
import { Cause, Effect, Exit, Result } from "effect";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import {
  OrganizationRuntime,
  type OrganizationRuntimeService,
} from "#/modules/runtime/organization-runtime.server";
import {
  PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import {
  openRuntimeWatch,
  openRuntimeWatchForOrganization,
} from "#/modules/runtime/runtime-watch.server";
import { runtimeWatchFrameFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import {
  Auth,
  type AuthService,
} from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

function unusedRuntime() {
  return asTestDouble<OrganizationRuntimeService>()({
    open: () => Effect.die("must not open runtime"),
  });
}

describe("openRuntimeWatch", () => {
  it("surfaces authorization failures before opening a runtime", async () => {
    const watch = await Effect.runPromise(
      Effect.result(
        openRuntimeWatch({
          request: new Request("http://localhost/api/runtime/events"),
          organizationSlug: "acme",
        }).pipe(
          Effect.provideService(
            Auth,
            asTestDouble<AuthService>()({
              getSession: () => Effect.succeed(null),
            }),
          ),
          Effect.provideService(Database, undefined as never),
          Effect.provideService(OrganizationRuntime, unusedRuntime()),
        ),
      ),
    );

    expect(Result.isFailure(watch)).toBe(true);
    if (Result.isFailure(watch)) {
      expect(watch.failure).toBeInstanceOf(Unauthorized);
    }
  });
});

describe("openRuntimeWatchForOrganization", () => {
  it("returns the authorized organization runtime state", async () => {
    const watch = await Effect.runPromise(
      Effect.result(
        openRuntimeWatchForOrganization({
          request: new Request("http://localhost/api/runtime/events"),
          organizationId: "org-1",
        }).pipe(
          Effect.provideService(
            OrganizationRuntime,
            asTestDouble<OrganizationRuntimeService>()({
              open: () => Effect.succeed({ status: "no_connection" }),
            }),
          ),
        ),
      ),
    );

    expect(watch).toEqual(Result.succeed({ status: "no_connection" }));
  });

  it("returns a connected stream and its lifecycle close operation", async () => {
    const frame = runtimeWatchFrameFixture({
      observed_at: "2026-08-18T00:00:00Z",
    });
    const request = new Request("http://localhost/api/runtime/events");
    const watchFn = vi.fn();
    const watch = await Effect.runPromise(
      Effect.result(
        openRuntimeWatchForOrganization({
          request,
          organizationId: "org-1",
        }).pipe(
          Effect.provideService(
            OrganizationRuntime,
            asTestDouble<OrganizationRuntimeService>()({
              open: () =>
                Effect.succeed({
                  status: "connected" as const,
                  connected: asTestDouble<PloyzSession>()({
                    watch: (options?: { readonly signal?: AbortSignal }) => {
                      watchFn(options);
                      return Effect.succeed(
                        (async function* () {
                          yield frame;
                        })(),
                      );
                    },
                  }),
                }),
            }),
          ),
        ),
      ),
    );

    expect(Result.isSuccess(watch)).toBe(true);
    if (Result.isFailure(watch) || watch.success.status !== "connected") {
      throw new Error("expected a connected watch");
    }
    const frames: RuntimeWatchView[] = [];
    for await (const next of watch.success.frames) frames.push(next);
    await Effect.runPromise(watch.success.close);

    expect(watchFn).toHaveBeenCalledWith({ signal: request.signal });
    expect(frames).toEqual([frame]);
  });

  it("surfaces runtime establishment failures", async () => {
    const watch = await Effect.runPromise(
      openRuntimeWatchForOrganization({
        request: new Request("http://localhost/api/runtime/events"),
        organizationId: "org-1",
      }).pipe(
        Effect.provideService(
          OrganizationRuntime,
          asTestDouble<OrganizationRuntimeService>()({
            open: () => Effect.fail(new Error("offline")),
          }),
        ),
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(watch)).toBe(true);
    if (Exit.isFailure(watch)) {
      const failure = Cause.squash(watch.cause);
      expect(failure).toBeInstanceOf(Error);
      expect(failure).not.toBeInstanceOf(RuntimeConnectionFailure);
      expect((failure as Error).message).toBe("offline");
    }
  });

  it("passes unreachable session errors through without wrapping", async () => {
    const sessionError = new PloyzProviderError({
      operation: "connect",
      cause: new Error("dial refused"),
    });
    const watch = await Effect.runPromise(
      Effect.result(
        openRuntimeWatchForOrganization({
          request: new Request("http://localhost/api/runtime/events"),
          organizationId: "org-1",
        }).pipe(
          Effect.provideService(
            OrganizationRuntime,
            asTestDouble<OrganizationRuntimeService>()({
              open: () =>
                Effect.succeed({
                  status: "unreachable" as const,
                  error: sessionError,
                }),
            }),
          ),
        ),
      ),
    );

    expect(watch).toEqual(
      Result.succeed({
        status: "unreachable",
        error: sessionError,
      }),
    );
  });

  it("maps SDK watch throws to RuntimeConnectionFailure", async () => {
    const watch = await Effect.runPromise(
      openRuntimeWatchForOrganization({
        request: new Request("http://localhost/api/runtime/events"),
        organizationId: "org-1",
      }).pipe(
        Effect.provideService(
          OrganizationRuntime,
          asTestDouble<OrganizationRuntimeService>()({
            open: () =>
              Effect.succeed({
                status: "connected" as const,
                connected: asTestDouble<PloyzSession>()({
                  watch: () =>
                    Effect.fail(
                      new RuntimeConnectionFailure({
                        cause: new Error("watch exploded"),
                      }),
                    ),
                }),
              }),
          }),
        ),
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(watch)).toBe(true);
    if (Exit.isFailure(watch)) {
      expect(Cause.squash(watch.cause)).toBeInstanceOf(RuntimeConnectionFailure);
    }
  });
});
