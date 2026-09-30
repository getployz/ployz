import { Outlet, createFileRoute } from "@tanstack/react-router";
import { PublicFooter, PublicHeader } from "#/routes/_public/-components/PublicChrome";

// Pathless group for the public pages: the lander at / (signed-out visitors) and /home (everyone), and
// /auth. They share one header and footer and stay light whatever the visitor's theme; LoginPanel sends
// sign-ins from here to /cloud.
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
