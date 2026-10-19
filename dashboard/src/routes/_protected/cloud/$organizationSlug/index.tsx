import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud/$organizationSlug/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/cloud/$organizationSlug/~", params, search: true, replace: true });
  },
});
