import { Link } from "@tanstack/react-router";
import { PloyzMark } from "./icons/ployz-logo";
import type { DashboardNavItem } from "./dashboard-navigation-model";
import { buttonVariants } from "./ui/button-variants";
import { cn } from "#/lib/utils";

/** The logo opens Projects. */
export function HomeLink({ organizationSlug, className }: { organizationSlug: string; className?: string }) {
  return (
    <Link to="/cloud/$organizationSlug/~" params={{ organizationSlug }} aria-label="Projects" title="Projects"
      className={cn(buttonVariants({ variant: "ghost", size: "icon-lg" }), className)}>
      <PloyzMark decorative className="size-7" />
    </Link>
  );
}

/**
 * Desktop: the logo, the Environment's places, then the organization's pages, the avatar at the bottom.
 * On organization pages the organization's pages lead; on an Environment they sit below a divider, at the bottom.
 */
export function Rail({ organizationSlug, places, organization, account }: {
  organizationSlug: string;
  places: DashboardNavItem[];
  organization: DashboardNavItem[];
  account: React.ReactNode;
}) {
  const railPlace = (place: DashboardNavItem) => <Place key={place.section} place={place}
    className="w-20 flex-col gap-1 rounded-lg py-2 text-xs whitespace-nowrap" />;
  return (
    <nav aria-label="Dashboard navigation"
      className="hidden w-22 shrink-0 flex-col items-center gap-1 overflow-y-auto border-r bg-background py-2 min-wf-nav:flex">
      <HomeLink organizationSlug={organizationSlug} className="mb-3" />
      {places.map(railPlace)}
      {places.length ? <div role="separator" className="mt-auto mb-1 h-px w-12 shrink-0 bg-border" /> : null}
      {organization.map(railPlace)}
      <div className={cn("pt-2", !places.length && "mt-auto")}>{account}</div>
    </nav>
  );
}

/** Phones: the same places along the bottom. */
export function PhoneTabBar({ places }: { places: DashboardNavItem[] }) {
  if (!places.length) return null;
  return (
    <nav aria-label="Environment places"
      className="flex shrink-0 border-t bg-background pb-[env(safe-area-inset-bottom)] min-wf-nav:hidden">
      {places.map((place) => <Place key={place.section} place={place} className="h-14 flex-1 flex-col gap-1 text-xs" />)}
    </nav>
  );
}

function Place({ place, className }: { place: DashboardNavItem; className: string }) {
  return (
    <Link to={place.to} params={place.params} search={place.search}
      // The model says which place is current; an exact match keeps a parent URL (the canvas) from also claiming it.
      activeOptions={{ exact: true }} aria-current={place.current ? "page" : undefined}
      className={cn(
        "flex items-center justify-center font-medium text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 aria-[current=page]:text-foreground",
        "min-wf-nav:aria-[current=page]:bg-muted",
        className,
      )}>
      <place.icon className="size-5" aria-hidden />
      {place.shortLabel ?? place.label}
    </Link>
  );
}
