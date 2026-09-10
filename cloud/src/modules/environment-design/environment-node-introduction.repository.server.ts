import "@tanstack/react-start/server-only";
import { compileEnvironmentIntent, redactEnvironmentIntent } from "@ployz/sdk/config";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { asRecord } from "#/lib/json";
import { environmentNodeIntroduction, environmentNodeIntroductionSecret } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { decodePersistedSavedEnvironmentIntent } from "./saved-intent";
import { loadCurrentEnvironmentState } from "./working-state-repository.server";

type IntroductionIdentity = Pick<typeof environmentNodeIntroduction.$inferInsert, "environmentId" | "nodeType" | "nodeId">;

/** Capture inside the creation transaction; neither public nor private history is updated later. */
export const captureEnvironmentNodeIntroduction = Effect.fn("EnvironmentDesign.captureEnvironmentNodeIntroduction")(
  function* (identity: IntroductionIdentity) {
    const { drizzle } = yield* Database;
    const { document, intent } = yield* loadCurrentEnvironmentState(identity.environmentId);
    const snapshot = compileEnvironmentIntent(identity.environmentId, redactEnvironmentIntent(intent)).nodeSnapshots
      .find((node) => node.nodeType === identity.nodeType && node.nodeId === identity.nodeId);
    const config = snapshot && asRecord(snapshot.config);
    if (!snapshot || !config) return yield* new Conflict({ message: "The new node has no authored introduction." });
    const [introduction] = yield* drizzle.insert(environmentNodeIntroduction).values({
      ...identity, organizationId: document.organizationId, nodeLineageId: snapshot.nodeLineageId,
      configVersion: snapshot.configVersion, config,
    }).returning();
    if (!introduction) return yield* Effect.die("PostgreSQL did not return the node introduction.");
    // ponytail: a full canonical document per introduction; narrow the immutable
    // snapshot only if measured history size warrants a separate node contract.
    yield* drizzle.insert(environmentNodeIntroductionSecret).values({ ...identity, authoredIntent: intent });
    return introduction;
  },
);

export const loadEnvironmentNodeIntroductionIntent = Effect.fn("EnvironmentDesign.loadEnvironmentNodeIntroductionIntent")(
  function* (identity: IntroductionIdentity) {
    const { drizzle } = yield* Database;
    const [baseline] = yield* drizzle.select().from(environmentNodeIntroductionSecret).where(and(
      eq(environmentNodeIntroductionSecret.environmentId, identity.environmentId),
      eq(environmentNodeIntroductionSecret.nodeType, identity.nodeType),
      eq(environmentNodeIntroductionSecret.nodeId, identity.nodeId),
    ));
    if (!baseline) return yield* new NotFound({ message: "Authored node introduction not found." });
    return yield* decodePersistedSavedEnvironmentIntent(baseline.authoredIntent);
  },
);
