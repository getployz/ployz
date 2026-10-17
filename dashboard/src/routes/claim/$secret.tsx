import { useMutation, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, Navigate, redirect, useLocation, useNavigate } from "@tanstack/react-router";
import { useAuth } from "#/auth/auth.hooks";
import { PloyzMark } from "#/components/icons/ployz-logo";
import { RelativeTime } from "#/components/relative-time";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { Spinner } from "#/components/ui/spinner";
import { toErrorMessage } from "#/lib/error-message";
import { plural } from "#/lib/plural";
import { prefetchRemote } from "#/collections/route-data";
import { claimOrganizationServerFn } from "#/routes/claim/-functions";
import { type Claim, claimQueryOptions } from "#/routes/claim/-queries";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

export const Route = createFileRoute("/claim/$secret")({
  loader: async ({ params, context }) => {
    await prefetchRemote(context, claimQueryOptions(params.secret));
    const claim = context.queryClient.getQueryData(claimQueryOptions(params.secret).queryKey);
    if (claim?.state === "claimed") {
      throw redirect({ to: "/cloud/$organizationSlug/~", params: { organizationSlug: claim.organization.slug }, replace: true });
    }
  },
  head: () => ({ meta: [{ title: "Keep your sandbox on Ployz" }] }),
  component: ClaimPage,
});

type Pending = Extract<Claim, { state: "pending" }>;

function ClaimPage() {
  const { secret } = Route.useParams();
  const claim = useSuspenseQuery(claimQueryOptions(secret)).data;
  const session = useAuth();
  // The loader's prefetch does not wait on client navigation, so a claimed state can arrive here instead.
  if (claim.state === "claimed") return <Navigate to="/cloud/$organizationSlug/~" params={{ organizationSlug: claim.organization.slug }} replace />;
  // An agent's anonymous session is signed out here, as on the sign-in routes: only a person keeps an Organization.
  const person = session !== null && session.user.isAnonymous !== true ? session : null;
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-12">
      <div className="w-full max-w-md">
        {claim.state === "expired" ? (
          <Expired signedIn={person !== null} />
        ) : person ? (
          <Keep claim={claim} email={person.user.email} />
        ) : (
          <ClaimSignIn claim={claim} />
        )}
      </div>
    </main>
  );
}

function Expired({ signedIn }: { signedIn: boolean }) {
  return (
    <div className="flex flex-col items-center gap-6 text-center">
      <PloyzMark className="size-11" decorative />
      <div className="flex flex-col gap-2">
        <h1 className="text-xl font-semibold">This sandbox expired</h1>
        <p className="text-sm text-muted-foreground">An agent's sandbox lasts a day unless someone keeps it. Run the agent again for a new one.</p>
      </div>
      <Link to={signedIn ? "/cloud" : "/auth"} className={buttonVariants({ variant: "outline" })}>
        {signedIn ? "Go to Cloud" : "Sign in"}
      </Link>
    </div>
  );
}

function Summary({ claim }: { claim: Pending }) {
  return (
    <p className="text-sm text-muted-foreground">
      {plural(claim.servers, "Server")} and {plural(claim.projects, "Project")}, gone <RelativeTime date={claim.expiresAt} /> unless someone keeps it.
    </p>
  );
}

function ClaimSignIn({ claim }: { claim: Pending }) {
  const callbackURL = useLocation({ select: (location) => location.href });
  return (
    <div className="flex flex-col gap-6">
      <LoginPanel
        title={`An agent built ${claim.organization.name}`}
        description="Sign in with GitHub to keep it."
        action="Continue with GitHub"
        callbackURL={callbackURL}
      />
      <div className="text-center">
        <Summary claim={claim} />
      </div>
    </div>
  );
}

function Keep({ claim, email }: { claim: Pending; email: string }) {
  const { secret } = Route.useParams();
  const navigate = useNavigate();
  const keep = useMutation({
    mutationFn: () => claimOrganizationServerFn({ data: { secret } }),
    // The session acts in the claimed Organization once it opens, so the whole document loads it.
    onSuccess: ({ slug }) => navigate({ to: "/cloud/$organizationSlug/~", params: { organizationSlug: slug }, reloadDocument: true }),
  });
  return (
    <div className="flex flex-col items-center gap-6 text-center">
      <PloyzMark className="size-11" decorative />
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">An agent built {claim.organization.name}</h1>
        <Summary claim={claim} />
        <p className="text-sm text-muted-foreground">You're signed in as {email}.</p>
      </div>
      <Button className="w-full" disabled={keep.isPending || keep.isSuccess} onClick={() => keep.mutate()}>
        {keep.isPending || keep.isSuccess ? <Spinner data-icon="inline-start" /> : null}
        Keep this Organization
      </Button>
      {keep.error ? <p role="alert" className="text-sm text-destructive">{toErrorMessage(keep.error, "Couldn't keep it. Try again.")}</p> : null}
    </div>
  );
}
