import {
  BarChart3Icon,
  BookOpenIcon,
  ChevronsUpDownIcon,
  CheckIcon,
  HomeIcon,
  LifeBuoyIcon,
  LogOutIcon,
  SunMoonIcon,
  UserIcon,
} from "lucide-react";
import { Link } from "@tanstack/react-router";
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
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
} from "#/components/ui/sidebar";
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

export default function DashboardAccountMenu({
  variant = "default",
  collapsed = false,
}: {
  variant?: "default" | "sidebar";
  collapsed?: boolean;
}) {
  const auth = useAuth();
  const signOut = useSignOut();
  const { userTheme, setTheme } = useTheme();

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

  const menuContent = (
    <DropdownMenuContent align="end" className="w-auto min-w-56">
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
        <DropdownMenuItem disabled>
          <UserIcon />
          Account settings
        </DropdownMenuItem>
        <DropdownMenuItem disabled>
          <BarChart3Icon />
          Project usage
        </DropdownMenuItem>
      </DropdownMenuGroup>

      <DropdownMenuSeparator />

      <DropdownMenuGroup>
        <DropdownMenuItem render={<Link to="/home" />}>
          <HomeIcon />
          Home Page
        </DropdownMenuItem>
        <DropdownMenuItem disabled>
          <BookOpenIcon />
          Documentation
        </DropdownMenuItem>
        <DropdownMenuItem disabled>
          <LifeBuoyIcon />
          Support
        </DropdownMenuItem>
      </DropdownMenuGroup>

      <DropdownMenuSeparator />

      <DropdownMenuGroup>
        <DropdownMenuItem
          onClick={() => {
            setTheme("light");
          }}
        >
          <SunMoonIcon />
          Light
          {userTheme === "light" ? <CheckIcon className="ml-auto" /> : null}
        </DropdownMenuItem>
        <DropdownMenuItem
          onClick={() => {
            setTheme("dark");
          }}
        >
          <SunMoonIcon />
          Dark
          {userTheme === "dark" ? <CheckIcon className="ml-auto" /> : null}
        </DropdownMenuItem>
        <DropdownMenuItem
          onClick={() => {
            setTheme("system");
          }}
        >
          <SunMoonIcon />
          Auto
          {userTheme === "system" ? <CheckIcon className="ml-auto" /> : null}
        </DropdownMenuItem>
        <DropdownMenuItem
          variant="destructive"
          onClick={() => void handleSignOut()}
        >
          <LogOutIcon />
          Log out
        </DropdownMenuItem>
      </DropdownMenuGroup>
    </DropdownMenuContent>
  );

  if (variant === "sidebar") {
    return (
      <SidebarMenu>
        <SidebarMenuItem>
          <DropdownMenu>
            <DropdownMenuTrigger
              render={
                <SidebarMenuButton
                  size="lg"
                  className={collapsed ? "justify-center" : undefined}
                />
              }
            >
              <Avatar size={collapsed ? "sm" : undefined}>
                <AvatarImage src={userImage} alt={userName} />
                <AvatarFallback>{userInitials}</AvatarFallback>
              </Avatar>
              {!collapsed && (
                <>
                  <div className="grid min-w-0 flex-1 text-left text-xs leading-tight">
                    <span className="truncate font-medium">{userName}</span>
                    <span className="truncate text-sidebar-foreground/70">
                      {userEmail}
                    </span>
                  </div>
                  <ChevronsUpDownIcon />
                </>
              )}
            </DropdownMenuTrigger>
            {menuContent}
          </DropdownMenu>
        </SidebarMenuItem>
      </SidebarMenu>
    );
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label="Open account menu"
        className="ml-auto outline-none focus-visible:ring-1 focus-visible:ring-ring/50"
      >
        <Avatar size="lg">
          <AvatarImage src={userImage} alt={userName} />
          <AvatarFallback>{userInitials}</AvatarFallback>
        </Avatar>
      </DropdownMenuTrigger>
      {menuContent}
    </DropdownMenu>
  );
}
