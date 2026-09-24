import { Effect, Schema } from "effect";
import { parseServiceConfig } from "@ployz/sdk/config";
import { Conflict } from "#/server/public-error";

export const deploymentSourcePinsSchema = Schema.Record(Schema.String, Schema.Struct({
  commitSha: Schema.String.check(Schema.isPattern(/^[0-9a-f]{40}$/)),
}));
export type DeploymentSourcePins = typeof deploymentSourcePinsSchema.Type;

/** Pins are execution inputs; authored branches remain in the frozen Saved config. */
export const validateDeploymentSourcePins = Effect.fn("Deployments.validateSourcePins")(
  function* (pins: DeploymentSourcePins, snapshots: readonly { nodeId: string; nodeType: string; config: unknown }[]) {
    for (const serviceId of Object.keys(pins)) {
      const service = snapshots.find(row => row.nodeId === serviceId && row.nodeType === "service");
      if (!service || parseServiceConfig(service.config).source.type !== "git") {
        return yield* new Conflict({ message: "A source pin must identify a frozen Git Service." });
      }
    }
    return pins;
  },
);
