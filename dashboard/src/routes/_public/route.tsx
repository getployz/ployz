import { Outlet, createFileRoute } from "@tanstack/react-router";
import { PublicFooter, PublicHeader } from "#/routes/_public/-components/PublicChrome";

// Pathless group for the public sign-in page, /auth: one header and footer, light whatever the visitor's
// theme; LoginPanel sends sign-ins from here to /cloud. The marketing pages live on the marketing site.
export const Route = createFileRoute("/_public")({ component: PublicLayout });

function PublicLayout() {
  return (
    <div className="light flex min-h-screen flex-col bg-background text-foreground">
      <PublicHeader />
      <Outlet />
      <PublicFooter />
    </div>
  );
}
