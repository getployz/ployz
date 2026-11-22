import { Effect } from "effect";
import { CHECKIN_MAX_BYTES } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, CheckinRefused } from "#/modules/machines/waiting/checkin.server";
import { readCappedBody } from "#/server/capped-body";
import { machineErrorResponse, machineRefusal, Unauthorized } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };

export const handleCheckin = Effect.fn("Checkin.handle")(function* (request: Request, observedIp: string) {
  const body = yield* Effect.tryPromise(() => readCappedBody(request, CHECKIN_MAX_BYTES)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
  if (body === null) return machineRefusal(413, "Check-in is over 64 KiB.");
  return yield* checkIn({ headers: request.headers, body, observedIp }).pipe(
    Effect.map((answer) => Response.json(answer, { headers: noStore })),
    Effect.catchTag("CheckinRefused", (error: CheckinRefused) =>
      Effect.succeed(machineRefusal(error.status, error.message, error.status === 429 ? { "retry-after": "1" } : {}))),
    Effect.catchTag("Unauthorized", (error: Unauthorized) => Effect.succeed(machineErrorResponse(error))),
  );
});
