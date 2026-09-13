import { getAuthSession } from "#/auth/auth";
import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected")({
  beforeLoad: async () => {
    const session = await getAuthSession();

    if (!session?.session || !session.user) {
      throw redirect({
        to: "/auth",
        replace: true,
      });
    }

    return {
      session: {
        ...session,
        session: {
          ...session.session,
          activeOrganizationSlug:
            session.session.activeOrganizationSlug ?? null,
        },
        user: session.user,
      }
    };
  },
  component: Outlet,
});
