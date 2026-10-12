import { createFileRoute, redirect } from "@tanstack/react-router";
import { Schema } from "effect";
import { getAuthSession } from "#/auth/auth";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

// Sign in and Deploy on the marketing site go straight to GitHub (/auth/github). This page is where the rest land: a
// protected page while signed out, Self-hosted Cloud's /, and better-auth's OAuth errors (onAPIError, ?error=).
const AuthSearch = Schema.Struct({ error: Schema.optional(Schema.String) });

const VIEWS = {
  start: { title: "Welcome to Ployz", description: "Keep the platform. Drop the bill.", action: "Continue with GitHub" },
  failed: { title: "Sign-in didn't finish", description: undefined, action: "Try again" },
} as const;

export const Route = createFileRoute("/_public/auth")({
  validateSearch: Schema.toStandardSchemaV1(AuthSearch),
  beforeLoad: async () => {
    const session = await getAuthSession();

    if (session?.session && session.user) {
      throw redirect({
        to: "/cloud",
        replace: true,
      });
    }
  },
  head: () => ({
    meta: [
      { title: "Sign in - Ployz" },
      {
        name: "description",
        content: "Sign in with GitHub to create a project, connect a repo or image, and start using Ployz Cloud.",
      },
    ],
  }),
  component: RouteComponent,
});

function RouteComponent() {
  const { error } = Route.useSearch();
  const view = VIEWS[error === undefined ? "start" : "failed"];
  return (
    <main className="flex flex-1 items-center justify-center bg-[radial-gradient(60rem_26rem_at_50%_0%,var(--color-changed-soft),transparent_70%)] px-5 pt-16 pb-28">
      <div className="w-full max-w-sm">
        <LoginPanel {...view} callbackURL="/cloud" />
      </div>
    </main>
  );
}
