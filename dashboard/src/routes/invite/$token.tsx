import { useMutation, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, useLocation, useNavigate } from "@tanstack/react-router";
import { useAuth } from "#/auth/auth.hooks";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { prefetchRemote } from "#/collections/route-data";
import { acceptInviteServerFn } from "#/modules/organization/members.functions";
import { type Invite, inviteQueryOptions } from "#/modules/organization/members.queries";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

export const Route = createFileRoute("/invite/$token")({
  loader: ({ params, context }) => prefetchRemote(context, inviteQueryOptions(params.token)),
  head: () => ({ meta: [{ title: "Join an organization on Ployz" }] }),
  component: InvitePage,
});

const UNUSABLE = {
  accepted: "This invite was used already.",
  revoked: "This invite was revoked.",
  expired: "This invite expired.",
} satisfies Record<Exclude<Invite["state"], "pending">, string>;

function InvitePage() {
  const { token } = Route.useParams();
  const invite = useSuspenseQuery(inviteQueryOptions(token)).data;
  const session = useAuth();
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-12">
      <div className="w-full max-w-md">
        {invite === null ? (
          <Notice title="This invite doesn't exist" detail="Check the link, or ask for a new one." />
        ) : invite.state !== "pending" ? (
          <Notice title={UNUSABLE[invite.state]} detail={`Ask someone in ${invite.organization} for a new one.`} />
        ) : session ? (
          <Join invite={invite} email={session.user.email} />
        ) : (
          <InviteSignIn invite={invite} />
        )}
      </div>
    </main>
  );
}

function Notice({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="flex flex-col items-center gap-2 text-center">
      <h1 className="text-xl font-semibold">{title}</h1>
      <p className="text-sm text-muted-foreground">{detail}</p>
    </div>
  );
}

function InviteSignIn({ invite }: { invite: Invite }) {
  const callbackURL = useLocation({ select: (location) => location.href });
  return (
    <LoginPanel
      title={`Join ${invite.organization}`}
      description={`${invite.invitedBy} invited you. Sign in to accept.`}
      action="Continue with GitHub"
      callbackURL={callbackURL}
    />
  );
}

function Join({ invite, email }: { invite: Invite; email: string }) {
  const { token } = Route.useParams();
  const navigate = useNavigate();
  const accept = useMutation({
    mutationFn: () => acceptInviteServerFn({ data: { token } }),
    // The session acts in the new Organization once it opens, so the whole document loads it.
    onSuccess: ({ slug }) => navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug: slug }, reloadDocument: true }),
  });
  return (
    <div className="flex flex-col items-center gap-6 text-center">
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Join {invite.organization}</h1>
        <p className="text-sm text-muted-foreground">
          {invite.invitedBy} invited you as {invite.role === "admin" ? "an admin" : "a member"}. You're signed in as {email}.
        </p>
      </div>
      <Button className="w-full" disabled={accept.isPending || accept.isSuccess} onClick={() => accept.mutate()}>
        {accept.isPending || accept.isSuccess ? <Spinner data-icon="inline-start" /> : null}
        Join {invite.organization}
      </Button>
      {accept.error ? <p role="alert" className="text-sm text-destructive">{toErrorMessage(accept.error, "Couldn't join. Try again.")}</p> : null}
    </div>
  );
}
