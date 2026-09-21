import { Outlet, createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected/cloud")({
  component: RouteComponent,
});

function RouteComponent() {
  return <Outlet />;
}
