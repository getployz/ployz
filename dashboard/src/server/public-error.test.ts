import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Cause, Data } from "effect";
import { SqlError, UnknownError } from "effect/unstable/sql/SqlError";
import { describe, expect, it, vi } from "vitest";
import { StoreRefused } from "#/modules/config-store/store.contract";
import {
  Conflict,
  encodePublicBoundaryError,
  encodePublicError,
  NotFound,
  publicErrorResponse,
  RateLimited,
  Validation,
} from "#/server/public-error";

class DatabaseFailure extends Data.TaggedError("DatabaseFailure")<{
  readonly cause: Error;
  readonly providerBody: string;
  readonly secret: string;
}> {}

class CollectionReadFailure extends Data.TaggedError("CollectionReadFailure")<{
  readonly cause: unknown;
}> {
  readonly publicErrorCategory = "internal" as const;
}

class MailRejected extends Data.TaggedError("MailRejected")<{
  readonly code: string;
  readonly cause: unknown;
}> {}

const failedQuery = () => new EffectDrizzleQueryError({
  query: "insert into machine_enrollment_token (token_hash) values ($1)",
  params: ["secret-param"],
  cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
});

class ValidationFailure extends Data.TaggedError("Validation")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

describe("encodePublicError", () => {
  it("maps capability not-found tags", () => {
    expect(
      encodePublicError(
        new NotFound({ message: "secret" }),
      ),
    ).toEqual({
      _tag: "PublicError",
      code: "NOT_FOUND",
      message: "The requested resource was not found.",
    });
  });

  it("encodes infrastructure failures as one plain redacted DTO", () => {
    const encoded = encodePublicError(
      new DatabaseFailure({
        cause: new Error("database host db.internal.test"),
        providerBody: "provider-token",
        secret: "application-secret",
      }),
    );

    expect(encoded).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
    expect(encoded).not.toBeInstanceOf(Error);
    expect(JSON.stringify(encoded)).not.toMatch(
      /application-secret|provider-token|db\.internal|stack|cause/u,
    );
  });

  it("maps capability validation failures without exposing their details", () => {
    expect(
      encodePublicError(
        new ValidationFailure({ message: "secret-bearing validation context" }),
      ),
    ).toEqual({
      _tag: "PublicError",
      code: "VALIDATION_FAILED",
      message: "The request is invalid.",
    });
  });

  it("sends a user-facing refusal's message and keeps every other one generic", () => {
    expect(encodePublicError(new Validation({ message: "Can't reach your servers.", userFacing: true }))).toEqual({
      _tag: "PublicError", code: "VALIDATION_FAILED", message: "Can't reach your servers.",
    });
    expect(encodePublicError(new Conflict({ message: "web is the Default Environment.", userFacing: true }))).toEqual({
      _tag: "PublicError", code: "CONFLICT", message: "web is the Default Environment.",
    });
    expect(encodePublicError(new Conflict({ message: "row 0190 locked by inngest run" })).message)
      .toBe("The request conflicts with the current state.");
    // Only refusals carry the flag through; an internal failure never does.
    expect(encodePublicError({ _tag: "DatabaseFailure", userFacing: true, message: "db.internal" }).message)
      .toBe("The request could not be completed.");
  });

  it("answers a rate limit with 429 and RATE_LIMITED", async () => {
    const response = publicErrorResponse(new RateLimited({ message: "Too many signups from this address.", userFacing: true }));
    expect(response.status).toBe(429);
    expect(await response.json()).toEqual({
      _tag: "PublicError", code: "RATE_LIMITED", message: "Too many signups from this address.",
    });
  });

  it("does not report a refusal with a public category", () => {
    const report = vi.fn();

    const response = publicErrorResponse(new Conflict({ message: "Taken." }), { report });

    expect(report).not.toHaveBeenCalled();
    expect(response.status).toBe(409);
  });

  it("reports a failed query once by its tags and SQLSTATE, never the row it wrote, and still answers 500", async () => {
    const report = vi.fn();
    const failedQuery = new EffectDrizzleQueryError({
      query: "insert into machine_enrollment_token (token_hash) values ($1)",
      params: ["secret-param"],
      cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
    });

    const response = publicErrorResponse(failedQuery, { report });

    expect(report).toHaveBeenCalledExactlyOnceWith("defect", "EffectDrizzleQueryError > SqlError > UnknownError (P0001)");
    expect(response.status).toBe(500);
    expect(await response.json()).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
  });

  it("does not report a request the validator refused", async () => {
    const report = vi.fn();

    const response = publicErrorResponse(new Error(JSON.stringify([{ message: "Expected a string" }])), { report });

    expect(report).not.toHaveBeenCalled();
    expect(response.status).toBe(422);
  });

  it("reports a failure with the internal category once by its tags, and still answers 500", async () => {
    const report = vi.fn();
    const failedRead = new CollectionReadFailure({ cause: failedQuery() });

    const response = publicErrorResponse(failedRead, { report });

    expect(report).toHaveBeenCalledExactlyOnceWith(
      "defect",
      "CollectionReadFailure > EffectDrizzleQueryError > SqlError > UnknownError (P0001)",
    );
    expect(response.status).toBe(500);
  });

  it("names a failure only by identifier tags and a SQLSTATE, whichever wrapper carries its own code", () => {
    const report = vi.fn();
    const leaky = { _tag: "row secret-param", code: "secret-param", cause: failedQuery() };

    publicErrorResponse(new MailRejected({ code: "mailbox_full", cause: leaky }), { report });

    expect(report).toHaveBeenCalledExactlyOnceWith(
      "defect",
      "MailRejected > ? > EffectDrizzleQueryError > SqlError > UnknownError (P0001)",
    );
  });

  it("reports a Store refusal that escaped unmapped, since it answers 500", () => {
    const report = vi.fn();

    const response = publicErrorResponse(
      new StoreRefused({ code: "not_found", message: "No Environment named secret-env", details: null }),
      { report },
    );

    expect(report).toHaveBeenCalledExactlyOnceWith("defect", "StoreRefused");
    expect(response.status).toBe(500);
  });

  it("reports a query that died by its kind and statement, never its params or message", () => {
    const report = vi.fn();
    const died = Cause.die(failedQuery());

    const error = encodePublicBoundaryError(died, { report });

    expect(report).toHaveBeenCalledExactlyOnceWith(
      "defect",
      "EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token",
    );
    expect(error.code).toBe("INTERNAL");
  });

  it("names a died failure that wraps a query by its tags and the query's statement", () => {
    const report = vi.fn();

    encodePublicBoundaryError(Cause.die(new CollectionReadFailure({ cause: failedQuery() })), { report });

    expect(report).toHaveBeenCalledExactlyOnceWith(
      "defect",
      "CollectionReadFailure > EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token",
    );
  });

  it("names no statement whose words sit in a literal or comment, and none when one is left open", () => {
    const named = (query: string) => {
      const report = vi.fn();
      encodePublicBoundaryError(
        Cause.die(new EffectDrizzleQueryError({ query, params: [], cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: {} }) })) })),
        { report },
      );
      return String(report.mock.calls[0]?.[1]);
    };

    expect(named("select 'from secret_literal' from t")).toMatch(/ in select t$/u);
    expect(named("select $tag$update secret_dollar$tag$ from t")).toMatch(/ in select t$/u);
    expect(named("select 1 /* from secret_comment */ from t")).toMatch(/ in select t$/u);
    expect(named("select 1 -- from secret_line\nfrom t")).toMatch(/ in select t$/u);
    expect(named("select E'it\\'s from secret_open' from t")).not.toMatch(/secret_open| in /u);
  });

  it("names every reason beside a died query safely, and the interruption too", () => {
    const report = vi.fn();
    const query = failedQuery();
    const cause = Cause.combine(
      Cause.combine(Cause.fail(new Error("safe-first secret-a")), Cause.die(query)),
      Cause.combine(Cause.die(new Error(query.message)), Cause.combine(Cause.fail(new DatabaseFailure({ cause: new Error("secret-d"), providerBody: "secret-b", secret: "secret-c" })), Cause.interrupt(3))),
    );

    encodePublicBoundaryError(cause, { report });

    const logged = report.mock.calls.map((call) => String(call[1])).join("\n");
    expect(report).toHaveBeenCalledTimes(2);
    expect(logged).toContain("EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token");
    expect(logged).toContain("DatabaseFailure");
    expect(logged).not.toMatch(/secret-|safe-first/u);
  });

  it("finds a died query in any branch of a Cause a failure wraps, and fails closed past what it reads", () => {
    const report = vi.fn();
    const nested = new CollectionReadFailure({ cause: Cause.combine(Cause.die(new Error("ordinary")), Cause.die(failedQuery())) });
    let deep = new Error("wrapper", { cause: failedQuery() });
    for (let depth = 0; depth < 100; depth += 1) deep = new Error(`wrapper ${depth}`, { cause: deep });

    encodePublicBoundaryError(Cause.die(nested), { report });
    encodePublicBoundaryError(Cause.die(deep), { report });

    const logged = report.mock.calls.map((call) => String(call[1])).join("\n");
    expect(logged).toContain("CollectionReadFailure");
    expect(logged).not.toMatch(/secret-|wrapper/u);
  });

  it("names a thrown Error that wraps a failed query by its class and the query", () => {
    const report = vi.fn();

    encodePublicBoundaryError(new Error("outer secret-outer", { cause: failedQuery() }), { report });

    expect(report).toHaveBeenCalledExactlyOnceWith(
      "defect",
      "EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token",
    );
  });

  it("reports full defect and interruption causes separately while redacting transport", async () => {
    const report = vi.fn();
    const defect = Cause.die(new Error("provider-token"));
    const defectError = encodePublicBoundaryError(defect, { report });
    const interruption = Cause.interrupt(7);
    const interruptionError = encodePublicBoundaryError(interruption, { report });

    expect(report).toHaveBeenNthCalledWith(1, "defect", defect);
    expect(report).toHaveBeenNthCalledWith(2, "interruption", interruption);
    expect(defectError).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
    expect(interruptionError).toEqual({
      _tag: "PublicError",
      code: "INTERNAL",
      message: "The request could not be completed.",
    });
  });
});
