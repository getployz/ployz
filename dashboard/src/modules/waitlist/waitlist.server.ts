import "@tanstack/react-start/server-only";

import { Cause, Data, Effect, Option, Schema, SchemaGetter } from "effect";
import { waitlist } from "#/modules/waitlist/tables";
import { errorEvidenceFrom } from "#/lib/error-evidence";
import { Database } from "#/server/database.server";
import { encodePublicError } from "#/server/public-error";

const email = Schema.String.check(
  Schema.isMaxLength(254),
  Schema.isPattern(/^[^\s@]+@[^\s@]+\.[^\s@]+$/),
).pipe(
  Schema.decode({
    decode: SchemaGetter.transform((value) => value.trim().toLowerCase()),
    encode: SchemaGetter.transform((value) => value),
  }),
  Schema.brand("WaitlistEmail"),
);

const WaitlistEnrollment = Schema.Struct({ email });
type WaitlistEnrollment = typeof WaitlistEnrollment.Type;

export class WaitlistDatabaseFailure extends Data.TaggedError(
  "WaitlistDatabaseFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

class WaitlistValidation extends Data.TaggedError("WaitlistValidation")<{
  readonly cause: unknown;
}> {
  readonly publicErrorCategory = "validation" as const;
}

function isDuplicateEnrollment(cause: { readonly cause: unknown }) {
  if (errorEvidenceFrom(cause).code === "23505") return true;
  if (!Cause.isCause(cause.cause)) return false;
  const failure = Cause.findErrorOption(cause.cause);
  return (
    Option.isSome(failure) && errorEvidenceFrom(failure.value).code === "23505"
  );
}

export const enrollWaitlist = Effect.fn("Waitlist.enroll")(function* (
  enrollment: WaitlistEnrollment,
) {
  const database = yield* Database;
  yield* database.drizzle
    .insert(waitlist)
    .values({ email: enrollment.email })
    .pipe(
      Effect.catchIf(isDuplicateEnrollment, () => Effect.void),
      Effect.tapError((cause) => Effect.logError("waitlist insert", cause)),
      Effect.mapError((cause) => new WaitlistDatabaseFailure({ cause })),
    );
  return { ok: true } as const;
});

function errorResponse(cause: unknown) {
  const error = encodePublicError(cause);
  return Response.json(
    { ok: false },
    { status: error.code === "VALIDATION_FAILED" ? 400 : 500 },
  );
}

export const handleWaitlistRequest = Effect.fn("Waitlist.handleRequest")(
  function* (request: Request) {
    const body = yield* Effect.promise(() =>
      request.json().catch(() => null),
    );
    const enrollment = yield* Effect.result(
      Schema.decodeUnknownEffect(WaitlistEnrollment)(body).pipe(
        Effect.mapError((cause) => new WaitlistValidation({ cause })),
      ),
    );
    if (enrollment._tag === "Failure") {
      return errorResponse(enrollment.failure);
    }
    return yield* Effect.match(enrollWaitlist(enrollment.success), {
      onFailure: errorResponse,
      onSuccess: Response.json,
    });
  },
);
