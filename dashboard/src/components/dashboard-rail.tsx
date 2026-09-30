import { Link } from "@tanstack/react-router";
import { PloyzMark } from "./icons/ployz-logo";
import type { DashboardNavItem, DashboardNavSection } from "./dashboard-navigation-model";
import { buttonVariants } from "./ui/button-variants";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./ui/tooltip";
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

// One row height in both widths, and every icon's center sits 28px from the rail's edge in both, so narrowing never moves it.
const row = "flex h-9 shrink-0 items-center gap-2.5 rounded-md px-2.5 text-sm font-medium";
const rowLink = "text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 aria-[current=page]:bg-muted aria-[current=page]:text-foreground";

/**
 * Desktop: the logo, the scope's places, on an Environment the way back to the organization below a divider, and the
 * account at the bottom. Each place is an icon beside its label, and the current place lists its page's sections; the
 * account row names you and the organization beside your avatar. `narrow` (wherever the canvas shows) leaves only the
 * icons and the avatar, each naming itself on hover, so the canvas gets the room.
 */
export function Rail({ organizationSlug, places, organization, narrow, account }: {
  organizationSlug: string;
  places: DashboardNavItem[];
  organization: DashboardNavItem[];
  narrow: boolean;
  account: React.ReactNode;
}) {
  const railPlace = (place: DashboardNavItem) => <RailPlace key={place.section} place={place} narrow={narrow} />;
  return (
    <nav aria-label="Dashboard navigation"
      className={cn(
        "hidden shrink-0 flex-col gap-1 overflow-x-hidden overflow-y-auto border-r bg-background p-2 whitespace-nowrap transition-[width] duration-200 ease-out motion-reduce:transition-none min-wf-nav:flex",
        narrow ? "w-14" : "w-56",
      )}>
      <HomeLink organizationSlug={organizationSlug} className="mb-3 size-10 shrink-0" />
      <TooltipProvider>
        {places.map(railPlace)}
        {organization.length ? <div role="separator" className="mx-2 mt-auto mb-1 h-px shrink-0 bg-border" /> : null}
        {organization.map(railPlace)}
      </TooltipProvider>
      <div className={cn("flex shrink-0", narrow ? "size-10 items-center justify-center" : "w-full", !organization.length && "mt-auto")}>{account}</div>
    </nav>
  );
}

function RailPlace({ place, narrow }: { place: DashboardNavItem; narrow: boolean }) {
  const icon = <place.icon className="size-5 shrink-0" aria-hidden />;
  if (!narrow && place.current && place.sections.length) {
    return (
      <div className="flex shrink-0 flex-col gap-0.5">
        {/* The place heads its sections, and the open section carries the current mark. */}
        <p className={cn(row, "text-foreground")}>{icon}{place.label}</p>
        <div className="ml-5 flex flex-col gap-0.5 border-l pl-2">
          {place.sections.map((section) => <SectionLink key={section.label} section={section}
            className={cn(row, rowLink, "h-8 font-normal aria-[current=page]:font-medium")} />)}
        </div>
      </div>
    );
  }
  const link = (
    <Link to={place.to} params={place.params} search={place.search}
      // The model says which place is current; an exact match keeps a parent URL (Architecture) from also claiming it.
      activeOptions={{ exact: true }} aria-current={place.current ? "page" : undefined}
      className={cn(row, rowLink, narrow && "justify-center px-0")}>
      {icon}
      <span className={narrow ? "sr-only" : "truncate"}>{place.label}</span>
    </Link>
  );
  if (!narrow) return link;
  return (
    <Tooltip>
      <TooltipTrigger render={link} />
      <TooltipContent side="right">{place.label}</TooltipContent>
    </Tooltip>
  );
}

function SectionLink({ section, className }: { section: DashboardNavSection; className: string }) {
  return (
    <Link to={section.to} params={section.params} search={section.search}
      activeOptions={{ exact: true }} aria-current={section.current ? "page" : undefined} className={className}>
      {section.label}
    </Link>
  );
}

/** Phones: the scope's places along the bottom. */
export function PhoneTabBar({ places }: { places: DashboardNavItem[] }) {
  return (
    <nav aria-label="Places"
      className="flex shrink-0 border-t bg-background pb-[env(safe-area-inset-bottom)] min-wf-nav:hidden">
      {places.map((place) => (
        <Link key={place.section} to={place.to} params={place.params} search={place.search}
          activeOptions={{ exact: true }} aria-current={place.current ? "page" : undefined}
          className="flex h-14 flex-1 flex-col items-center justify-center gap-1 text-xs font-medium text-muted-foreground outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 aria-[current=page]:text-foreground">
          <place.icon className="size-5" aria-hidden />
          {place.label}
        </Link>
      ))}
    </nav>
  );
}

/** Phones: the current place's sections, in a strip under the top bar. */
export function PhoneSections({ places }: { places: DashboardNavItem[] }) {
  const place = places.find((candidate) => candidate.current && candidate.sections.length);
  if (!place) return null;
  return (
    <nav aria-label={`${place.label} sections`}
      className="flex h-10 shrink-0 gap-5 overflow-x-auto border-b bg-background px-4 min-wf-nav:hidden">
      {place.sections.map((section) => <SectionLink key={section.label} section={section}
        className="-mb-px flex items-center border-b-2 border-transparent text-sm font-medium whitespace-nowrap text-muted-foreground outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 aria-[current=page]:border-foreground aria-[current=page]:text-foreground" />)}
    </nav>
  );
}
