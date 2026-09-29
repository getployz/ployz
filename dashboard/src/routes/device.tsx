import { useState, useTransition, type FormEvent } from "react";
import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { authClient } from "#/auth/auth-client";
import { useAuth } from "#/auth/auth.hooks";
import { Button } from "#/components/ui/button";
import { Input } from "#/components/ui/input";
import { Spinner } from "#/components/ui/spinner";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

const DeviceSearch = Schema.Struct({ user_code: Schema.optional(Schema.String) });

// `ployz login` sends the user here with its code (RFC 8628 verification_uri_complete).
export const Route = createFileRoute("/device")({
  validateSearch: Schema.toStandardSchemaV1(DeviceSearch),
  head: () => ({ meta: [{ title: "Sign in the Ployz CLI" }] }),
  component: DevicePage,
});

type Outcome = "approved" | "denied";

function DevicePage() {
  const session = useAuth();
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-12">
      <div className="w-full max-w-md">
        {/* Signed out: LoginPanel returns here, code included, after sign-in. */}
        {session ? <DeviceApproval email={session.user.email} organization={session.session.activeOrganizationSlug ?? null} /> : <LoginPanel />}
      </div>
    </main>
  );
}

function DeviceApproval({ email, organization }: { email: string; organization: string | null }) {
  const search = Route.useSearch();
  const [code, setCode] = useState(search.user_code ?? "");
  const [outcome, setOutcome] = useState<Outcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  function decide(decision: Outcome) {
    setError(null);
    startTransition(async () => {
      const userCode = code.trim();
      // Opening the code claims it for this user; only then may this user decide it.
      const claimed = await authClient.device({ query: { user_code: userCode } });
      if (claimed.error) {
        setError(claimed.error.error_description);
        return;
      }
      if (claimed.data.status !== "pending") {
        setError("This code was already used. Run ployz login again.");
        return;
      }
      const decided = decision === "approved"
        ? await authClient.device.approve({ userCode })
        : await authClient.device.deny({ userCode });
      if (decided.error) {
        setError(decided.error.error_description);
        return;
      }
      setOutcome(decision);
    });
  }

  function approve(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    decide("approved");
  }

  if (outcome !== null) {
    return (
      <div className="flex flex-col items-center gap-2 text-center">
        <h1 className="text-xl font-semibold">
          {outcome === "approved" ? "Your terminal is signed in" : "Sign-in denied"}
        </h1>
        <p className="text-sm text-muted-foreground">
          {outcome === "approved" ? "You can close this tab and return to your terminal." : "The terminal that asked was not signed in."}
        </p>
      </div>
    );
  }

  return (
    <form className="flex flex-col items-center gap-6 text-center" onSubmit={approve}>
      <div className="flex flex-col gap-1">
        <h1 className="text-xl font-semibold">Sign in the Ployz CLI</h1>
        <p className="text-sm text-muted-foreground">
          Signed in as {email}{organization ? ` in ${organization}` : ""}. Approve only if this code matches your terminal.
        </p>
      </div>
      <Input
        aria-label="Code shown in your terminal"
        className="text-center font-mono tracking-widest"
        value={code}
        onChange={(event) => setCode(event.target.value)}
        placeholder="ABCD1234"
        autoComplete="off"
        required
      />
      <div className="flex w-full gap-2">
        <Button type="button" variant="outline" className="flex-1" disabled={pending} onClick={() => decide("denied")}>
          Deny
        </Button>
        <Button type="submit" className="flex-1" disabled={pending}>
          {pending ? <Spinner data-icon="inline-start" /> : null}
          Approve
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
