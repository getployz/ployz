import { useEffect, useState, useTransition } from "react";
import { useHydrated, useRouter } from "@tanstack/react-router";
import { authClient } from "#/auth/auth-client";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { PloyzMark } from "#/components/icons/ployz-logo";
import { Button } from "#/components/ui/button";
import { Spinner } from "#/components/ui/spinner";

export function LoginPanel({
  title,
  description,
  action,
  callbackURL,
}: {
  title: string;
  description?: string;
  action: string;
  /** Where GitHub sign-in returns: a path on this origin. */
  callbackURL: string;
}) {
  const isHydrated = useHydrated();
  const router = useRouter();
  const [error, setError] = useState<string | null>(null);
  const [isGithubPending, setIsGithubPending] = useState(false);
  const [isRandomAccountPending, startRandomAccountTransition] = useTransition();
  const isPending = isGithubPending || isRandomAccountPending;

  // Back from GitHub through the browser's back button: the page comes from the bfcache with the spinner still on.
  useEffect(() => {
    const reset = () => setIsGithubPending(false);
    window.addEventListener("pageshow", reset);
    return () => window.removeEventListener("pageshow", reset);
  }, []);

  function createRandomDevAccount() {
    setError(null);
    startRandomAccountTransition(async () => {
      try {
        const id = crypto.randomUUID();
        const response = await authClient.signUp.email({
          name: `Local developer ${id.slice(0, 8)}`,
          email: `dev-${id}@ployz.local`,
          password: crypto.randomUUID(),
        });

        if (response.error) {
          const detail = response.error.message || response.error.code;
          setError(detail ? `Could not create a dev account: ${detail}` : "Could not create a dev account.");
          return;
        }

        await router.navigate({ to: "/cloud", search: { welcome: true }, reloadDocument: true });
      } catch {
        setError("Could not reach the auth server. Check that the dev server is running.");
      }
    });
  }

  return (
    <div className="flex w-full flex-col items-center gap-6 text-center">
      <PloyzMark className="size-11" decorative />
      <div className="flex flex-col gap-2">
        <h1 className="text-3xl font-semibold tracking-tight text-balance">{title}</h1>
        {description ? <p className="text-balance text-muted-foreground">{description}</p> : null}
      </div>
      <div className="flex w-full flex-col gap-2">
        <Button
          variant="ink"
          size="lg"
          className="w-full"
          nativeButton={false}
          aria-busy={isGithubPending || undefined}
          render={<a href={`/auth/github?${new URLSearchParams({ callbackURL })}`} />}
          onClick={() => setIsGithubPending(true)}
        >
          {isGithubPending ? <Spinner data-icon="inline-start" /> : <GitHubMarkIcon data-icon="inline-start" />}
          {action}
        </Button>
        {import.meta.env.DEV ? (
          <Button
            type="button"
            disabled={!isHydrated || isPending}
            size="lg"
            variant="outline"
            className="w-full"
            onClick={createRandomDevAccount}
          >
            {isRandomAccountPending ? <Spinner data-icon="inline-start" /> : null}
            Create random dev account
          </Button>
        ) : null}
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
      </div>
    </div>
  );
}
