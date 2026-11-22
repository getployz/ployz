import type { JsonValue } from "@ployz/sdk";
import { Data } from "effect";

/** A Store refusal as the Store words it: the RPC error vocabulary, never the rejected value. */
export type StoreRefusal = { code: string; message: string; details: JsonValue };

/**
 * The Store refused a call, in the RPC error vocabulary: `conflict` carries fresh state, `not_found` names what's
 * missing. Cloud fails with it; the browser's writer rejects with it.
 */
export class StoreRefused extends Data.TaggedError("StoreRefused")<StoreRefusal> {
  get refusal(): StoreRefusal {
    return { code: this.code, message: this.message, details: this.details };
  }
}

/** A refusal's value: what a Store call refuses with. */
export const refusal = (code: string, message: string, details: JsonValue = null): StoreRefusal => ({ code, message, details });

/** A command or tool's answer when it refuses. */
export const refused = (code: string, message: string, details: JsonValue = null) =>
  ({ ok: false, refusal: refusal(code, message, details) }) as const;

/** What a call says when Cloud can't reach the Cluster's Servers. A screen matches the start to ask again a while. */
export const SERVERS_UNANSWERED_START = "Your servers aren't answering";
export const SERVERS_UNANSWERED_MESSAGE = `${SERVERS_UNANSWERED_START}. Try again once they are.`;

/** The refusal for Servers that don't answer. Each caller keeps its own `code`: the CLI and the agent read it. */
export const serversUnanswered = (code: "unavailable" | "unanswered") => refusal(code, SERVERS_UNANSWERED_MESSAGE);
