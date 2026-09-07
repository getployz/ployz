import type { ReactNode } from "react";
import { Link } from "@tanstack/react-router";
import { XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { buttonVariants } from "#/components/ui/button-variants";
import { DrawerClose } from "#/components/ui/drawer";
import { useIsMobile } from "#/hooks/use-mobile";
import { ENVIRONMENT_INDEX_ROUTE_TO } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-route-paths";

type CanvasInspectorHeaderParams = {
  organizationSlug: string;
  projectSlug: string;
  environmentSlug: string;
};

const closeContent = (
  <>
    <XIcon />
    <span className="sr-only">Close</span>
  </>
);

export function CanvasInspectorHeader({
  params,
  children,
}: {
  params: CanvasInspectorHeaderParams;
  children: ReactNode;
}) {
  const isMobile = useIsMobile();

  return (
    <div className="flex items-center justify-between gap-4 border-b px-6 py-4">
      <div className="min-w-0 flex-1">{children}</div>
      {isMobile ? (
        <DrawerClose render={<Button variant="ghost" size="icon" />}>
          {closeContent}
        </DrawerClose>
      ) : (
        <Link
          to={ENVIRONMENT_INDEX_ROUTE_TO}
          params={{
            organizationSlug: params.organizationSlug,
            projectSlug: params.projectSlug,
            environmentSlug: params.environmentSlug,
          }}
          search={(prev) => prev}
          viewTransition={{ types: ["canvas-inspector-close"] }}
          className={buttonVariants({ variant: "ghost", size: "icon" })}
        >
          {closeContent}
        </Link>
      )}
    </div>
  );
}
