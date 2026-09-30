import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Building2Icon, ChevronsUpDownIcon, HouseIcon, LogOutIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  getSignOutErrorMessage,
  useAuth,
  useSignOut,
} from "#/auth/auth.hooks";
import { useTheme } from "#/components/theme-provider";
import { Avatar, AvatarFallback, AvatarImage } from "#/components/ui/avatar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import {
  getDashboardDestination,
  type DashboardScope,
} from "#/components/dashboard-navigation-model";
import { useDashboardSection } from "#/components/use-dashboard-section";
import { organizationStateQueryOptions } from "#/modules/environment-design/workspace.queries";
import { toast } from "sonner";
import { Result } from "effect";

function getInitials(name: string) {
  return name
    .split(" ")
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase() ?? "")
    .join("");
}

const themes = [
  { value: "system", label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
] as const;

/**
 * The account menu: switching organization, Theme, Home (the public page) and Log out. The organization's
 * pages are places. As a `row` (the wide rail) its trigger names you and the organization beside the
 * avatar; otherwise it's the avatar alone.
 */
export default function DashboardAccountMenu({
  scope,
  side = "bottom",
  variant = "avatar",
}: {
  scope: DashboardScope;
  side?: "bottom" | "right";
  variant?: "avatar" | "row";
}) {
  const auth = useAuth();
  const signOut = useSignOut();
  const navigate = useNavigate();
  const { userTheme, setTheme } = useTheme();
  const section = useDashboardSection();
  const organizations = useQuery(organizationStateQueryOptions(scope.organizationSlug)).data?.organizations ?? [];

  async function handleSignOut() {
    const result = await signOut();

    if (Result.isFailure(result)) {
      toast.error(getSignOutErrorMessage(result.failure));
    }
  }

  if (!auth?.user) {
    return null;
  }

  const userName = auth.user.name || auth.user.email || "Account";
  const userEmail = auth.user.email || "";
  const userInitials = getInitials(userName);
  const userImage = auth.user.image ?? undefined;
  const organizationName = organizations.find((candidate) => candidate.slug === scope.organizationSlug)?.name ?? scope.organizationSlug;

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label="Open account menu"
        title={variant === "row" ? undefined : userName}
        render={variant === "row"
          // The narrow rail's 40px avatar slot, widened: the avatar and the links above it hold still as the rail changes width.
          ? <button type="button" className="flex h-10 w-full min-w-0 items-center gap-2.5 rounded-md px-1 text-left outline-none hover:bg-muted focus-visible:ring-3 focus-visible:ring-ring/50 aria-expanded:bg-muted" />
          : <Button variant="ghost" size="icon-lg" />}
      >
        <Avatar>
          <AvatarImage src={userImage} alt={userName} />
          <AvatarFallback>{userInitials}</AvatarFallback>
        </Avatar>
        {variant === "row" ? (
          <>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-sm font-medium">{userName}</span>
              <span className="block truncate text-xs text-muted-foreground">{organizationName}</span>
            </span>
            <ChevronsUpDownIcon className="size-4 shrink-0 text-muted-foreground" />
          </>
        ) : null}
      </DropdownMenuTrigger>
      <DropdownMenuContent side={side} align="end" className="w-auto min-w-56">
        <div className="flex items-center gap-3 p-2">
          <Avatar size="lg">
            <AvatarImage src={userImage} alt={userName} />
            <AvatarFallback>{userInitials}</AvatarFallback>
          </Avatar>
          <div className="min-w-0 flex-1">
            <p className="truncate text-sm font-medium">{userName}</p>
            <p className="truncate text-xs text-muted-foreground">{userEmail}</p>
          </div>
        </div>

        {organizations.length > 1 ? (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuGroup>
              <DropdownMenuSub>
                <DropdownMenuSubTrigger>
                  <Building2Icon />
                  Switch organization
                </DropdownMenuSubTrigger>
                <DropdownMenuSubContent>
                  <DropdownMenuRadioGroup value={scope.organizationSlug} onValueChange={(slug: string) => {
                    void navigate(getDashboardDestination({ kind: "all", organizationSlug: slug }, section));
                  }}>
                    {organizations.map((candidate) => (
                      <DropdownMenuRadioItem key={candidate.id} value={candidate.slug}>
                        {candidate.name}
                      </DropdownMenuRadioItem>
                    ))}
                  </DropdownMenuRadioGroup>
                </DropdownMenuSubContent>
              </DropdownMenuSub>
            </DropdownMenuGroup>
          </>
        ) : null}

        <DropdownMenuSeparator />

        <DropdownMenuGroup>
          <DropdownMenuLabel>Theme</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={userTheme} onValueChange={setTheme}>
            {themes.map((theme) => (
              <DropdownMenuRadioItem key={theme.value} value={theme.value}>{theme.label}</DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuGroup>

        <DropdownMenuSeparator />

        <DropdownMenuItem render={<Link to="/home" />}>
          <HouseIcon />
          Home
        </DropdownMenuItem>
        <DropdownMenuItem
          variant="destructive"
          onClick={() => void handleSignOut()}
        >
          <LogOutIcon />
          Log out
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
