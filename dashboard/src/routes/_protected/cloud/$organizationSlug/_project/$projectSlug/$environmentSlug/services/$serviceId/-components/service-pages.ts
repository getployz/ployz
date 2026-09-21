import { Effect, Option, Schema } from "effect";

export const SERVICE_PAGES = [
  { id: "settings", label: "Settings" },
  { id: "variables", label: "Variables" },
] as const;

export type ServicePage = (typeof SERVICE_PAGES)[number]["id"];

export const servicePageSchema = Schema.Literals(SERVICE_PAGES.map((page) => page.id));

export const serviceSearchSchema = Schema.Struct({
  tab: Schema.optional(servicePageSchema.pipe(
    Schema.catchDecoding(() => Effect.succeed(Option.some("settings" as const))),
  )),
});
