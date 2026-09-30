import { useState } from "react";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { Button } from "#/components/ui/button";
import { Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyTitle } from "#/components/ui/empty";
import { openOrCreateOrganizationServerFn } from "#/modules/organization/organization-state.functions";

export const Route = createFileRoute("/_protected/cloud/")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({
    welcome: Schema.optional(Schema.Boolean.pipe(
      Schema.catchDecoding(() => Effect.succeed(Option.some(false))),
    )),
  })),
  beforeLoad: ({ context, cause, search }) => {
    if (cause === "preload") return;

    const slug = context.session.session.activeOrganizationSlug;

    // Acting in no Organization (its own was deleted): the page offers one.
    if (!slug) return;

    throw redirect({
      to: search.welcome ? "/cloud/$organizationSlug/new" : "/cloud/$organizationSlug/~",
      search: {},
      replace: true,
      params: { organizationSlug: slug },
    });
  },
  component: NoOrganization,
});

/** A signed-in user with no Organization: open their next one, or create one. */
function NoOrganization() {
  const navigate = useNavigate();
  const [pending, setPending] = useState(false);
  async function open() {
    setPending(true);
    try {
      const slug = await openOrCreateOrganizationServerFn();
      await navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug: slug }, replace: true, reloadDocument: true });
    } finally {
      setPending(false);
    }
  }
  return (
    <div className="flex min-h-dvh items-center justify-center p-6">
      <Empty>
        <EmptyHeader>
          <EmptyTitle>You're not in an organization</EmptyTitle>
          <EmptyDescription>Projects, servers and billing live in an organization.</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <Button disabled={pending} onClick={() => void open()}>Create an organization</Button>
        </EmptyContent>
      </Empty>
    </div>
  );
}
