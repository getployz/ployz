import {
  Link,
  useMatch,
} from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { SearchXIcon, UserXIcon } from "lucide-react";
import { buttonVariants } from "#/components/ui/button-variants";
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

const decodeFormerMember = Schema.decodeUnknownOption(Schema.Struct({ formerOrganization: Schema.String }));

export function NotFoundPage({ data }: { data?: unknown }) {
  return Option.match(decodeFormerMember(data), {
    onNone: () => <MissingPage />,
    onSome: ({ formerOrganization }) => <NoLongerMember organization={formerOrganization} />,
  });
}

function NoLongerMember({ organization }: { organization: string }) {
  return (
    <main className="flex min-h-screen items-center justify-center px-4 py-16">
      <Empty className="max-w-xl">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <UserXIcon />
          </EmptyMedia>
          <EmptyTitle>Membership ended</EmptyTitle>
          <EmptyDescription>Ask someone in {organization} for a new invite.</EmptyDescription>
        </EmptyHeader>
        <EmptyContent className="sm:flex-row sm:justify-center">
          <Link to={CloudRoute.to} className={buttonVariants()}>
            Go to Cloud
          </Link>
        </EmptyContent>
      </Empty>
    </main>
  );
}

function MissingPage() {
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
          <Link to={homeRoute} className={buttonVariants()}>
            Go home
          </Link>
        </EmptyContent>
      </Empty>
    </main>
  );
}
