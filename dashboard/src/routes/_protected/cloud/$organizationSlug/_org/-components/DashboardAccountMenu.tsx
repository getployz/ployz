import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Building2Icon, LogOutIcon } from "lucide-react";
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
import { useDashboardNavigation, useDashboardSection } from "#/components/use-dashboard-section";
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

/** The avatar menu: organization pages, switching organization, Theme and Log out. */
export default function DashboardAccountMenu({
  scope,
  side = "bottom",
}: {
  scope: DashboardScope;
  side?: "bottom" | "right";
}) {
  const auth = useAuth();
  const signOut = useSignOut();
  const navigate = useNavigate();
  const { userTheme, setTheme } = useTheme();
  const { organization } = useDashboardNavigation(scope);
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

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label="Open account menu"
        title={userName}
        render={<Button variant="ghost" size="icon-lg" />}
      >
        <Avatar>
          <AvatarImage src={userImage} alt={userName} />
          <AvatarFallback>{userInitials}</AvatarFallback>
        </Avatar>
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

        <DropdownMenuSeparator />

        <DropdownMenuGroup>
          {organization.map((item) => (
            <DropdownMenuItem key={item.section}
              render={<Link to={item.to} params={item.params} search={item.search}
                activeOptions={{ exact: true }} aria-current={item.current ? "page" : undefined} />}>
              <item.icon />
              {item.label}
            </DropdownMenuItem>
          ))}
          {organizations.length > 1 ? (
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
          ) : null}
        </DropdownMenuGroup>

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
