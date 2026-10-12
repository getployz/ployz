import { Outlet, createFileRoute } from "@tanstack/react-router";
import { prefetchRemote } from "#/collections/route-data";
import { marketingSiteQueryOptions } from "#/modules/marketing/marketing-site.queries";
import { PublicHeader } from "#/routes/_public/-components/PublicChrome";

// Pathless group for the public sign-in page, /auth: the marketing site's header, light whatever the visitor's theme.
export const Route = createFileRoute("/_public")({
  loader: async ({ context }) => {
    await prefetchRemote(context, marketingSiteQueryOptions());
  },
  component: PublicLayout,
});

function PublicLayout() {
  return (
    <div className="light flex min-h-screen flex-col bg-background text-foreground">
      <PublicHeader />
      <Outlet />
    </div>
  );
}
