import { createFileRoute, Outlet } from "@tanstack/react-router";

/** The Environment's Deployments page: its Runs and its Versions, each a section of the rail. */
export const Route = createFileRoute("/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments")({
  component: Outlet,
});
