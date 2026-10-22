import { useRouter } from "@tanstack/react-router";
import { CloudAlertIcon } from "lucide-react";
import { useEffect } from "react";
import { Button } from "#/components/ui/button";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
} from "#/components/ui/empty";

/** How often a page that failed to load tries again on its own. */
const RETRY_MS = 5_000;

/**
 * A page whose loading failed, such as while Cloud's database restarts. It tries again every few seconds, so the page
 * comes back by itself once Cloud can serve it. The error itself goes to the server log, or the browser console when
 * rendering threw it.
 */
export function ErrorPage() {
  const router = useRouter();
  useEffect(() => {
    // A try skips while a load runs: invalidating mid-load aborts it, so a slow loader would never finish.
    const retry = setInterval(() => {
      if (!router.state.isLoading) void router.invalidate();
    }, RETRY_MS);
    return () => clearInterval(retry);
  }, [router]);

  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-16">
      <Empty className="max-w-xl">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <CloudAlertIcon />
          </EmptyMedia>
          {/* DESIGN.md rule 4: an error state keeps the page's h1. */}
          <h1 className="text-2xl font-semibold">This page couldn't load</h1>
          <EmptyDescription>Ployz tries again every few seconds, and the page comes back once it loads.</EmptyDescription>
        </EmptyHeader>
        <EmptyContent className="sm:flex-row sm:justify-center">
          <Button onClick={() => void router.invalidate()}>Try again</Button>
        </EmptyContent>
      </Empty>
    </main>
  );
}
