import { useState, useTransition, type FormEvent } from "react";
import { createFileRoute, useLocation } from "@tanstack/react-router";
import { useServerFn } from "@tanstack/react-start";
import { Button } from "#/components/ui/button";
import { Label } from "#/components/ui/label";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Spinner } from "#/components/ui/spinner";
import { connectAgentServerFn, connectChoicesServerFn } from "#/modules/agent/connect.functions";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

// An agent's OAuth authorize request lands here twice: signed out to sign in, then signed in to grant access.
export const Route = createFileRoute("/connect")({
  head: () => ({ meta: [{ title: "Connect an agent to Ployz" }] }),
  loader: () => connectChoicesServerFn(),
  component: ConnectPage,
});

type Organization = { readonly id: string; readonly slug: string; readonly name: string };

function ConnectPage() {
  const choices = Route.useLoaderData();
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-12">
      <div className="w-full max-w-md">
        {choices === null ? <ConnectSignIn /> : <ConnectConsent email={choices.email} organizations={choices.organizations} />}
      </div>
    </main>
  );
}

function ConnectSignIn() {
  const callbackURL = useLocation({ select: (location) => location.href });
  return <LoginPanel title="Sign in to connect an agent" description="Your agent will act as you in one Organization." action="Continue with GitHub" callbackURL={callbackURL} />;
}

function ConnectConsent({ email, organizations }: { email: string; organizations: ReadonlyArray<Organization> }) {
  const decide = useServerFn(connectAgentServerFn);
  const [organizationId, setOrganizationId] = useState(organizations[0]?.id ?? "");
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  function answer(accept: boolean) {
    setError(null);
    startTransition(async () => {
      try {
        const decided = await decide({ data: { organizationId, accept, oauthQuery: window.location.search.slice(1) } });
        window.location.href = decided.url;
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : "Ployz couldn't record your answer. Try again.");
      }
    });
  }

  function allow(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    answer(true);
  }

  if (organizations.length === 0) {
    return (
      <div className="flex flex-col items-center gap-2 text-center">
        <h1 className="text-xl font-semibold">No Organization to connect</h1>
        <p className="text-sm text-muted-foreground">Signed in as {email}. Create or join an Organization, then connect your agent again.</p>
      </div>
    );
  }

  return (
    <form className="flex flex-col gap-6" onSubmit={allow}>
      <div className="flex flex-col gap-1 text-center">
        <h1 className="text-xl font-semibold">Connect an agent to Ployz</h1>
        <p className="text-sm text-muted-foreground">
          Signed in as {email}. The agent can read and change this Organization as you. Destroys still wait for your approval.
        </p>
      </div>
      <div className="flex flex-col gap-2">
        <Label htmlFor="connect-organization">Organization</Label>
        <NativeSelect id="connect-organization" value={organizationId} onChange={(event) => setOrganizationId(event.target.value)}>
          {organizations.map((organization) => (
            <NativeSelectOption key={organization.id} value={organization.id}>{organization.name}</NativeSelectOption>
          ))}
        </NativeSelect>
      </div>
      <div className="flex w-full gap-2">
        <Button type="button" variant="outline" className="flex-1" disabled={pending} onClick={() => answer(false)}>
          Deny
        </Button>
        <Button type="submit" className="flex-1" disabled={pending}>
          {pending ? <Spinner data-icon="inline-start" /> : null}
          Allow
        </Button>
      </div>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
    </form>
  );
}
