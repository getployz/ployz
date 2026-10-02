import { cn } from "#/lib/utils";

// The frame around /auth (the _public layout): the wordmark, the header and the footer. The marketing pages
// live on the marketing site, proxied on this origin (see marketing-proxy.server.ts).

function Wordmark({ className }: { className?: string }) {
  return <span className={cn("font-bold tracking-[-0.04em]", className)}>ployz</span>;
}

export function PublicHeader() {
  return (
    <header className="sticky top-0 z-40 border-b border-border bg-background">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-6 px-5">
        {/* A full page load: / is the marketing site, served by the server, not a client route. */}
        <a href="/" className="text-xl">
          <Wordmark />
        </a>
      </div>
    </header>
  );
}

export function PublicFooter() {
  return (
    <footer className="border-t border-border px-5 pt-12 pb-10">
      <div className="mx-auto flex max-w-6xl flex-col gap-8">
        <Wordmark className="text-7xl leading-none md:text-9xl" />
        <div className="font-mono text-xs text-muted-foreground">Keep the platform. Drop the bill.</div>
      </div>
    </footer>
  );
}
