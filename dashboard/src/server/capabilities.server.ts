import { Effect } from "effect";
import { AppConfig } from "#/server/config.server";

/** Whether this Cloud bills: a hosted one has Polar, a Self-hosted one has none. */
export const billingEnabled = Effect.map(AppConfig, (config) => config.polar.mode === "hosted");
