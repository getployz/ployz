import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import { MenuIcon } from "lucide-react";
import { AppSidebar } from "#/components/app-sidebar";
import { NavigationSwitcher } from "#/components/navigation-switcher";
import { Button } from "#/components/ui/button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
  SheetTrigger,
} from "#/components/ui/sheet";
import DashboardAccountMenu from "#/routes/_protected/cloud/$organizationSlug/_org/-components/DashboardAccountMenu";

export default function DashboardHeader() {
  const [menuOpen, setMenuOpen] = useState(false);
  const params = useParams({ strict: false });
  const { organizationSlug, projectSlug, environmentSlug } = params;

  if (!organizationSlug) {
    return null;
  }

  const scope = {
    organizationSlug,
    ...(projectSlug && environmentSlug
      ? { kind: "environment" as const, projectSlug, environmentSlug }
      : { kind: "all" as const }),
  };

  return (
    <div className="flex h-full items-center px-4">
      <div className="flex w-full min-w-0 items-center gap-2 min-wf-nav:hidden">
        <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
          <SheetTrigger
            render={
              <Button
                variant="ghost"
                size="icon"
                aria-label="Open navigation"
              />
            }
          >
            <MenuIcon />
          </SheetTrigger>
          <SheetContent
            side="left"
            showCloseButton={false}
            className="w-72"
          >
            <SheetHeader className="sr-only">
              <SheetTitle>Navigation</SheetTitle>
              <SheetDescription>
                Navigate dashboard sections.
              </SheetDescription>
            </SheetHeader>
            <AppSidebar
              scope={scope}
              projection="mobile"
              onNavigate={() => setMenuOpen(false)}
            />
          </SheetContent>
        </Sheet>
        <div className="min-w-0 flex-1">
          <NavigationSwitcher projection="mobile" />
        </div>
        <DashboardAccountMenu />
      </div>

      <div className="hidden w-full min-w-0 items-center gap-3 min-wf-nav:flex">
        <NavigationSwitcher />
        <div className="ml-auto flex items-center gap-1">
          <Button variant="ghost" size="sm" disabled>
            Help
          </Button>
          <Button variant="ghost" size="sm" disabled>
            Docs
          </Button>
        </div>
      </div>
    </div>
  );
}
