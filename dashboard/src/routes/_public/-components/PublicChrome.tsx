import { useSuspenseQuery } from "@tanstack/react-query";
import { Button } from "#/components/ui/button";
import { marketingSiteQueryOptions } from "#/modules/marketing/marketing-site.queries";

// The marketing site's header (ployz-site's chrome/Header.astro) without its Product menu, so /auth reads as part of
// that site. Every link is a full page load: the marketing pages are proxied on this origin (marketing-proxy.server.ts),
// and /auth/github is a server redirect. A Self-hosted Cloud has no marketing pages, so it keeps only the wordmark
// (to /, which signs in there) and the sign-in links.
const LINKS = [
  { label: "Pricing", href: "/home#pricing" },
  { label: "Docs", href: "/docs" },
  { label: "Blog", href: "/blog" },
];

export function PublicHeader() {
  const { data: marketingSite } = useSuspenseQuery(marketingSiteQueryOptions());
  return (
    <header className="sticky top-0 z-40 border-b border-border bg-background">
      <div className="mx-auto flex h-14 max-w-6xl items-center gap-4 px-5 sm:gap-6 md:grid md:grid-cols-3">
        <a href={marketingSite ? "/home" : "/"} className="relative -top-0.5 text-xl font-bold tracking-tighter md:justify-self-start">
          ployz
        </a>
        {marketingSite && (
          <nav aria-label="Sections" className="hidden items-center gap-6 text-sm text-muted-foreground sm:flex md:justify-self-center">
            {LINKS.map((link) => (
              <a key={link.href} href={link.href} className="hover:text-foreground">
                {link.label}
              </a>
            ))}
          </nav>
        )}
        <div className="ml-auto flex items-center gap-4 md:col-start-3 md:justify-self-end">
          <a href="/auth/github" className="hidden text-sm text-muted-foreground hover:text-foreground sm:inline">
            Sign in
          </a>
          <Button variant="ink" nativeButton={false} render={<a href="/auth/github" />}>
            Deploy
          </Button>
        </div>
      </div>
    </header>
  );
}
