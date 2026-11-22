import { Effect } from "effect";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { BuildGrantUnavailable, Conflict, Forbidden, NotFound, statusForRefusal, Unavailable, Validation } from "#/server/public-error";

/**
 * A Store refusal as a user's request answers it: the public error of the status `statusForRefusal` gives the CLI, in its fixed
 * copy, since a Store message can name an ID. A 401 stays a 500, since the user's session already passed and the Store
 * means Cloud's own credential, and so does a 501, which has no public error.
 */
export function publicRefusal(error: StoreRefused) {
  const { message } = error;
  switch (statusForRefusal(error.code)) {
    case 422:
      return new Validation({ message });
    case 404:
      return new NotFound({ message });
    case 409:
      return new Conflict({ message });
    case 403:
      return new Forbidden({ message });
    case 503:
      return new Unavailable({ message });
    default:
      return error;
  }
}

/** `effect`, answering a Store refusal with its own status rather than a 500. */
export const refusedPublicly = <A, E, R>(effect: Effect.Effect<A, E, R>) =>
  effect.pipe(Effect.mapError((error) => error instanceof StoreRefused ? publicRefusal(error) : error));

/**
 * The Store's refusal as a GitHub build runner's API answers it. The runner holds no session, so `unauthenticated` is
 * its own credential refused (403), and any code it has no answer for reads as the grant being unavailable.
 */
export const runnerRefusal = (error: StoreRefused) => {
  const { message } = error;
  switch (error.code) {
    case "unauthenticated": return new Forbidden({ message });
    case "not_found": return new NotFound({ message });
    case "conflict": return new Conflict({ message });
    case "invalid_argument": return new Validation({ message });
    default: return new BuildGrantUnavailable({ cause: error });
  }
};
