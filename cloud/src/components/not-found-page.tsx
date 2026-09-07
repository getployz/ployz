import {
  Link,
  useMatch,
} from "@tanstack/react-router";
import { SearchXIcon } from "lucide-react";
import { SiteAction } from "#/components/marketing/SiteAction";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";
import { Route } from "#/routes/__root";
import { Route as CloudRoute } from "#/routes/_protected/cloud/index";
import { Route as ProtectedRoute } from "#/routes/_protected/route";
import { Route as OrganizationHomeRoute } from "#/routes/_protected/cloud/$organizationSlug/_org/~/index";

export function NotFoundPage() {
  const protectedMatch = useMatch({
    from: ProtectedRoute.id,
    shouldThrow: false,
  });

  const organizationHomeMatch = useMatch({
    from: OrganizationHomeRoute.id,
    shouldThrow: false,
  });

  // `/cloud` is the correct "home" destination for real protected misses, but
  // invalid organization slugs also flow through the protected shell before they
  // throw `notFound()`. We therefore only send users to `/cloud` when the
  // not-found happened inside the protected area without already resolving to a
  // concrete organization route.
  const homeRoute =
    protectedMatch && !organizationHomeMatch ? CloudRoute.to : Route.to;

  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-16">
      <Empty className="max-w-xl">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <SearchXIcon />
          </EmptyMedia>
          <EmptyTitle>Page not found</EmptyTitle>
          <EmptyDescription>
            That route does not exist or has moved.
          </EmptyDescription>
        </EmptyHeader>
        <EmptyContent className="sm:flex-row sm:justify-center">
          <SiteAction to={homeRoute}>Go home</SiteAction>
          <Link
            to="/docs"
            className="text-sm text-muted-foreground underline-offset-4 hover:underline"
          >
            Open docs
          </Link>
        </EmptyContent>
      </Empty>
    </main>
  );
}
