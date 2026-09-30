import { useState, type ReactNode } from "react";
import { Link, useHydrated, useMatch } from "@tanstack/react-router";
import { buttonVariants } from "#/components/ui/button-variants";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { cn } from "#/lib/utils";
import { LoginPanel } from "#/routes/_public/-components/LoginPanel";

// The frame every signed-out page shares (the _public layout): the wordmark, the header and the footer.

function Wordmark({ className }: { className?: string }) {
  return <span className={cn("font-bold tracking-[-0.04em]", className)}>ployz</span>;
}

/**
 * A link to /auth that, once the page is interactive, opens the sign-in panel in a dialog over the page
 * instead. The dialog portals to <body>, outside the layout's `.light` root, so it carries `light` itself.
 */
export function LoginDialog({ className, children }: { className?: string; children: ReactNode }) {
  const isHydrated = useHydrated();
  const [open, setOpen] = useState(false);

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <Link
        to="/auth"
        aria-haspopup="dialog"
        aria-expanded={open}
        className={className}
        onClick={(event) => {
          if (!isHydrated) return;
          event.preventDefault();
          setOpen(true);
        }}
      >
        {children}
      </Link>
      <DialogContent className="light sm:max-w-md">
        <DialogHeader className="sr-only">
          <DialogTitle>Sign in</DialogTitle>
          <DialogDescription>Sign in with GitHub to continue.</DialogDescription>
        </DialogHeader>
        <LoginPanel />
      </DialogContent>
    </Dialog>
  );
}

export function PublicHeader() {
  // On /auth the page is the sign-in, so the frame drops its own sign-in links.
  const onSignIn = useMatch({ from: "/_public/auth", shouldThrow: false });
  return (
    <header className="sticky top-0 z-40 border-b border-border bg-background">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-6 px-5">
        <Link to="/" hash="top" className="text-xl">
          <Wordmark />
        </Link>
        <nav aria-label="Sections" className="hidden items-center gap-5 text-sm text-muted-foreground md:flex">
          <Link to="/" hash="start" className="hover:text-foreground">
            How it works
          </Link>
          <Link to="/" hash="pricing" className="hover:text-foreground">
            Pricing
          </Link>
          <Link to="/" hash="questions" className="hover:text-foreground">
            Questions
          </Link>
        </nav>
        {onSignIn ? null : (
          <div className="ml-auto flex items-center gap-2">
            <LoginDialog className={buttonVariants({ variant: "ghost" })}>Sign in</LoginDialog>
            <LoginDialog className={buttonVariants({ variant: "ink" })}>Deploy</LoginDialog>
          </div>
        )}
      </div>
    </header>
  );
}

export function PublicFooter() {
  const onSignIn = useMatch({ from: "/_public/auth", shouldThrow: false });
  return (
    <footer className="border-t border-border px-5 pt-12 pb-10">
      <div className="mx-auto flex max-w-6xl flex-col gap-8">
        <Wordmark className="text-7xl leading-none md:text-9xl" />
        <div className="flex flex-wrap items-center gap-x-6 gap-y-3 font-mono text-xs text-muted-foreground">
          <span>Keep the platform. Drop the bill.</span>
          {onSignIn ? null : (
            <LoginDialog className="hover:text-foreground md:ml-auto">Sign in</LoginDialog>
          )}
        </div>
      </div>
    </footer>
  );
}
