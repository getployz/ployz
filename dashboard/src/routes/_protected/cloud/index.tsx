import { createFileRoute, notFound, redirect } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";

export const Route = createFileRoute("/_protected/cloud/")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    welcome: Schema.optional(Schema.Boolean.pipe(
      Schema.catchDecoding(() => Effect.succeed(Option.some(false))),
    )),
  })),
  beforeLoad: ({ context, cause, search }) => {
    if (cause === "preload") return;

    const slug = context.session.session.activeOrganizationSlug;

    if (!slug) {
      throw notFound();
    }

    throw redirect({
      to: search.welcome ? "/cloud/$organizationSlug/new" : "/cloud/$organizationSlug/~",
      search: {},
      replace: true,
      params: { organizationSlug: slug },
    });
  },
});
