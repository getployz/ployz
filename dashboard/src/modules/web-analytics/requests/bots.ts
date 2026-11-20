import { Effect, Schema } from "effect";
import { botsAmong } from "../classify";
import type { Range } from "../model";
import { Params, readPageviews, readRequests, type Scope } from "./statement";

/**
 * Past this many distinct user agents in one window the rarest go unclassified and count as people. The bot list
 * travels in the URL as 64-bit hashes, and 20 000 of them stay far under ClickHouse's URI limit.
 */
const DISTINCT_AGENTS = 20_000;

const Agent = Schema.Struct({ ua: Schema.String, hash: Schema.String });

/**
 * The hashes of the bot user agents in a window. ClickHouse lists the window's distinct agents, the `isbot` list names
 * the bots among them here, and a figure's query then drops `cityHash64(Ua)` in this list. `pageviewsOnly` looks only at
 * the agents behind page requests, which the pageview table holds alone.
 */
export const botHashes = Effect.fn("WebAnalytics.bots")(function* (scope: Scope, range: Range, pageviewsOnly: boolean) {
  const params = new Params();
  const read = pageviewsOnly ? readPageviews : readRequests;
  const agents = yield* read(scope, params, [], `
    SELECT Ua AS ua, toString(cityHash64(Ua)) AS hash FROM scoped
    WHERE Timestamp >= ${params.instant(range.start)} AND Timestamp < ${params.instant(range.end)}
    GROUP BY Ua ORDER BY count() DESC, Ua LIMIT ${DISTINCT_AGENTS}`, Agent);
  return botsAmong(agents).map(({ hash }) => hash);
});
