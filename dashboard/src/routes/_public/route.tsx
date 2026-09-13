import { Outlet, createFileRoute, useLocation } from "@tanstack/react-router";
import Footer from "#/routes/_public/-components/Footer";
import Header from "#/routes/_public/-components/Header";

export const Route = createFileRoute("/_public")({
  component: RouteComponent,
});

function RouteComponent() {
  const isHome = useLocation({
    select: (location) =>
      location.pathname === "/" ||
      location.pathname === "/home" ||
      location.pathname === "/home/",
  });
  if (isHome) return <Outlet />;

  return (
    <div className="flex min-h-screen flex-col">
      <Header />
      <div className="flex flex-1 flex-col">
        <Outlet />
      </div>
      <Footer />
    </div>
  );
}
