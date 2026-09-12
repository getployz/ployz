import {
  useEffect,
  useState,
  useTransition,
  type FormEvent,
  type MouseEvent,
  type ReactNode,
} from "react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import {
  Link,
  useHydrated,
  useRouter,
  useRouterState,
} from "@tanstack/react-router";
import { Spinner } from "#/components/ui/spinner";
import { authClient } from "#/auth/auth-client";
import {
  buttonVariants,
  type ButtonVariants,
} from "#/components/ui/button-variants";
import { cn } from "#/lib/utils";

export function LoginPanel() {
  const isHydrated = useHydrated();
  const router = useRouter();
  const isPublicShell = useRouterState({
    select: (state) =>
      state.matches.some((match) => match.routeId === "/_public"),
  });
  const currentLocation = useRouterState({
    select: (state) => state.location.href,
  });
  const callbackURL = isPublicShell ? "/cloud" : currentLocation;
  const [error, setError] = useState<string | null>(null);
  const [isGithubPending, setIsGithubPending] = useState(false);
  const [isRandomAccountPending, startRandomAccountTransition] =
    useTransition();
  const isPending = isGithubPending || isRandomAccountPending;

  useEffect(() => {
    const reset = () => setIsGithubPending(false);
    window.addEventListener("pageshow", reset);
    return () => window.removeEventListener("pageshow", reset);
  }, []);

  async function signInWithGithub(event: FormEvent<HTMLFormElement>) {
    if (!isHydrated) {
      return;
    }

    event.preventDefault();
    if (isPending) return;
    setError(null);
    setIsGithubPending(true);
    try {
      const response = await authClient.signIn.social({
        provider: "github",
        callbackURL,
      });

      if (response.error) {
        const detail = response.error.message || response.error.code;
        setError(
          detail
            ? `Could not start GitHub sign-in: ${detail}`
            : "Could not start GitHub sign-in.",
        );
        setIsGithubPending(false);
      }
    } catch {
      setError(
        "Could not reach the auth server. Check that the dev server is running.",
      );
      setIsGithubPending(false);
    }
  }

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
          setError(
            detail
              ? `Could not create a dev account: ${detail}`
              : "Could not create a dev account.",
          );
          return;
        }

        await router.navigate({ to: "/cloud", reloadDocument: true });
      } catch {
        setError(
          "Could not reach the auth server. Check that the dev server is running.",
        );
      }
    });
  }

  return (
    <div className="flex flex-col items-center gap-6 px-2 py-4 text-center">
      <span className="flex size-12 items-center justify-center rounded-2xl bg-primary text-base font-semibold text-primary-foreground shadow-sm">
        P
      </span>
      <div className="flex flex-col gap-1">
        <h2 className="text-xl font-semibold">Login to Ployz</h2>
        <p className="text-sm text-muted-foreground">
          Push-to-deploy on your own servers.
        </p>
      </div>
      <div className="flex w-full flex-col gap-2">
        <form method="post" action="/api/auth/github" onSubmit={signInWithGithub}>
          <input type="hidden" name="callbackURL" value={callbackURL} />
          <Button
            type="submit"
            disabled={isPending}
            size="lg"
            className="w-full"
          >
            {isGithubPending ? (
              <Spinner data-icon="inline-start" />
            ) : (
              <GitHubMarkIcon data-icon="inline-start" />
            )}
            {isGithubPending ? "Connecting to GitHub…" : "Continue with GitHub"}
          </Button>
        </form>
        {import.meta.env.DEV ? (
          <Button
            type="button"
            disabled={!isHydrated || isPending}
            size="lg"
            variant="outline"
            className="w-full"
            onClick={createRandomDevAccount}
          >
            {isRandomAccountPending ? (
              <Spinner data-icon="inline-start" />
            ) : null}
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

type LoginDialogProps = Pick<ButtonVariants, "size" | "variant"> & {
  children?: ReactNode;
  className?: string;
};

export default function LoginDialog({
  children = "Sign in",
  className,
  size = "sm",
  variant = "default",
}: LoginDialogProps) {
  const isHydrated = useHydrated();
  const [open, setOpen] = useState(false);

  function handleTriggerClick(event: MouseEvent<HTMLAnchorElement>) {
    if (!isHydrated) {
      return;
    }

    event.preventDefault();
    setOpen(true);
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <Link
        to="/auth"
        preload="intent"
        aria-expanded={open}
        aria-haspopup="dialog"
        className={cn(buttonVariants({ size, variant }), className)}
        onClick={handleTriggerClick}
      >
        {children}
      </Link>
      <DialogContent className="max-w-md">
        <DialogHeader className="sr-only">
          <DialogTitle>Sign in</DialogTitle>
          <DialogDescription>
            Sign in with GitHub to continue.
          </DialogDescription>
        </DialogHeader>
        <LoginPanel />
      </DialogContent>
    </Dialog>
  );
}
