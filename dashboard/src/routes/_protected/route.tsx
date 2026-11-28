import { getAuthSession } from "#/auth/auth";
import { Outlet, createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/_protected")({
  beforeLoad: async ({ location }) => {
    const session = await getAuthSession();

    if (!session?.session || !session.user) {
      // Back here once signed in, as a link from GitHub to a pull request's preview expects.
      throw redirect({
        to: "/auth",
        search: { callbackURL: location.href },
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
