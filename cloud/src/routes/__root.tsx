import {
  HeadContent,
  Outlet,
  Scripts,
  createRootRouteWithContext,
} from "@tanstack/react-router";
import { TanStackRouterDevtoolsPanel } from "@tanstack/react-router-devtools";
import { TanStackDevtools } from "@tanstack/react-devtools";
import { FormDevtoolsPanel } from "@tanstack/react-form-devtools";
import { ReactQueryDevtoolsPanel } from "@tanstack/react-query-devtools";
import { ThemeProvider } from "../components/theme-provider";
import { Toaster } from "../components/ui/sonner";
import { getAuthSession } from "../auth/auth";
import {
  getServerThemeClassName,
  getTheme,
  type UserTheme,
} from "../utils/theme";
import appCss from "../styles.css?url";
import type { QueryClient } from "@tanstack/react-query";
import { getTableSyncBaseUrl } from "#/electric/table-sync-url";
export const Route = createRootRouteWithContext<{
  queryClient: QueryClient;
}>()({
  beforeLoad: async () => ({
    tableSyncBaseUrl: await getTableSyncBaseUrl(),
  }),
  loader: async () => {
    const theme = getTheme();
    const session = await getAuthSession();

    return {
      theme,
      session,
    };
  },
  head: () => ({
    meta: [
      {
        charSet: "utf-8",
      },
      {
        name: "viewport",
        content: "width=device-width, initial-scale=1",
      },
      {
        title: "Ployz",
      },
      {
        name: "application-name",
        content: "Ployz",
      },
    ],
    links: [
      {
        rel: "stylesheet",
        href: appCss,
      },
      {
        rel: "icon",
        type: "image/svg+xml",
        href: "/assets/logo-mark.svg",
      },
    ],
  }),
  component: RootComponent,
});

function RootComponent() {
  const { theme } = Route.useLoaderData();
  return (
    <RootDocument theme={theme}>
      <ThemeProvider theme={theme}>
        <Outlet />
        <Toaster />
      </ThemeProvider>
    </RootDocument>
  );
}

function RootDocument({
  children,
  theme,
}: {
  children: React.ReactNode;
  theme: UserTheme;
}) {
  return (
    <html
      lang="en"
      className={getServerThemeClassName(theme)}
      suppressHydrationWarning
    >
      <head>
        <HeadContent />
      </head>
      <body className="font-sans antialiased [overflow-wrap:anywhere]">
        {children}
        <TanStackDevtools
          eventBusConfig={{
            connectToServerBus: true,
          }}
          config={{
            hideUntilHover: true,
            position: "bottom-right",
          }}
          plugins={[
            {
              name: "Tanstack Router",
              render: <TanStackRouterDevtoolsPanel />,
            },
            {
              name: "TanStack Query",
              render: <ReactQueryDevtoolsPanel />,
            },
            {
              name: "TanStack Form",
              render: (_el, props) => <FormDevtoolsPanel {...props} />,
            },
          ]}
        />
        <Scripts />
      </body>
    </html>
  );
}
