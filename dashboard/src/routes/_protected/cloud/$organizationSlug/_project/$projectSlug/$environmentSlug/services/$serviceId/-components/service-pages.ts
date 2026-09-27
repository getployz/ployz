import { Effect, Option, Schema } from "effect";

export const SERVICE_PAGES = [
  { id: "deployments", label: "Deployments" },
  { id: "variables", label: "Variables" },
  { id: "logs", label: "Logs" },
  { id: "settings", label: "Settings" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(servicePageSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("settings" as const))),
  )),
});
