import { Effect, Option, Schema } from "effect";
import { UPTIME_RANGES } from "#/modules/uptime/uptime-status";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "monitoring", label: "Monitoring" },
  { id: "settings", label: "Settings" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(servicePageSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("settings" as const))),
  )),
  range: Schema.optional(Schema.Literals(UPTIME_RANGES).pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.none())),
  )),
});
