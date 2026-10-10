import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import { API } from "typescript/unstable/sync";
import type { Node, SourceFile } from "typescript/unstable/ast";
import {
  isAwaitExpression,
  isCallExpression,
  isIdentifier,
  isImportDeclaration,
  isMethodDeclaration,
  isNamedImports,
  isObjectLiteralExpression,
  isPropertyAccessExpression,
  isPropertyAssignment,
  isShorthandPropertyAssignment,
  isStringLiteral,
  isVariableDeclaration,
} from "typescript/unstable/ast/is";
import { dataSources } from "./data-sources";

const root = process.cwd();
const SRC = join(root, "src");
const DATA_FILE = /^collections\/[^/]+\.ts$|\.collection\.ts$|\.queries\.ts$|\.stream\.ts$/;
const CREATES_SOURCE = /\b(createApiCollection|createChangeCollection|queryCollectionOptions|localOnlyCollectionOptions|liveQueryCollectionOptions|queryOptions|infiniteQueryOptions|createCollection)\s*[<(]|\bqueryFn\s*:/;
const SPINNER = /<Spinner\b|Loader2Icon|animate-spin/;
/** Spinners mean a write is in flight or a runtime process is running, never a read. */
const SPINNER_FILES = {
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel.tsx": "creating a Branch in the Config Store",
  "components/ui/spinner.tsx": "the primitive",
  "components/ui/sonner.tsx": "promise toasts for writes",
  "components/confirm-dialog.tsx": "confirm in flight",
  "components/deletion-dialog.tsx": "deletion in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/SyncDialog.tsx": "sync in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/create-environment-dialog.tsx": "create in flight",
  "components/service-create-command.tsx": "create in flight",
  "components/service-source-selector.tsx": "sync and submit in flight",
  "form/index.tsx": "submit in flight",
  "routes/_protected/cloud/$organizationSlug/-components/store-teardown-section.tsx": "the removal Deployment running",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/domain-row.tsx": "Setting up while the Cluster Domain sync runs; Issuing certificate while the certificate is ordered",
  "routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection.tsx": "Setting up while the sync runs; Check again until the sync lands",
  "routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection.tsx": "reset in flight",
  "routes/_protected/cloud/$organizationSlug/_org/~/billing.tsx": "portal opening",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/add-server-dialog.tsx": "command mint in flight",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/server-services-section.tsx": "the Drain running on the Server",
  "routes/_public/-components/LoginPanel.tsx": "sign-in in flight",
  "routes/device.tsx": "device approval in flight",
};

const READ_SERVER_FN = /\b(get|load|list|preview|search|resolve|read)[A-Z]\w*ServerFn\b/;
const SERVER_FN_FILE = /[.-]functions\.ts$|\.server\.ts$/;
/** Reads that are one step of a user command (preview, evidence, wait for completion), not page state. */
const COMMAND_READ_FILES = {
  "components/service-source-selector.tsx": "resolve a pasted public repository before connecting it",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/remove-server-section.tsx": "gather data-loss evidence, then wait for the confirmed removal",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/stray-namespaces.tsx": "gather a Namespace's data-loss evidence before the user confirms its removal",
  "routes/_protected/cloud/$organizationSlug/_org/-components/forget-servers-dialog.tsx": "try every Server and list what goes before the user confirms forgetting them",
};

const NETWORK = /\bfetch\(|new EventSource\(/;
/** Raw network access outside data files and server code. */
const NETWORK_FILES = {
  "modules/github/github-observation.api.ts": "server-only GitHub API client",
  "modules/inngest/client.ts": "server-only Inngest REST lookup of one run's status for the volume run sweep",
};

/** Remote Reads a loader cannot prefetch, and what warms them instead. */
const ON_DEMAND_READS = {
  githubFileSearchQueryOptions: "searches as the user types",
  githubRepoAccessQueryOptions: "read together with the install URL when a repository picker opens",
  githubInstallUrlQueryOptions: "read together with repository access when a repository picker opens",
  githubBranchesQueryOptions: "depends on the repository the user just picked",
  githubBuildRepositoriesQueryOptions: "calls GitHub per repository; Organization Settings › Builds fills it in after hydration",
  strayNamespacesQueryOptions: "asks about the Namespaces the Servers report live, known only once the Runtime Watch answers",
};

/** Hook files outside data files that await the server without making UI wait on it. */
const HOOK_FILES_NOT_COMMANDS = {
  "modules/config-store/store-write.ts": "the Store writer owns the per-Environment optimistic queue; edits show before they save, and UI awaiting a commit is listed itself",
};

/** UI that waits for the server, and why. Everything else applies writes optimistically. */
const COMMAND_FILES = {
  "components/service-create-command.tsx": "the server assigns a new project's id and slug, and its canvas can't show the new Service before the Store has it",
  "components/service-source-selector.tsx": "resolving a public repository and syncing GitHub are external",
  "routes/_protected/cloud/$organizationSlug/_org/~/billing.tsx": "the billing portal involves money",
  "routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger.tsx": "deleting an organization is destructive and waits on its Servers letting go",
  "routes/_protected/cloud/$organizationSlug/_org/-components/forget-servers-dialog.tsx": "forgetting the Servers is destructive: it tries every Server again and forgets only when none answers",
  "routes/_protected/cloud/index.tsx": "the server creates the new organization and its slug, which the page then opens",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-settings.tsx": "renaming a Project changes its URLs: the page opens the new one once the Store has it",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/create-environment-dialog.tsx": "over the Store the dialog stays open until the name is accepted, then opens the new environment",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/remove-server-section.tsx": "removing a server is destructive and waits on the runtime",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/stray-namespaces.tsx": "removing a Namespace no Project owns deletes its Volumes' data: it reads what goes before the user confirms, then waits on the Servers",
  "routes/_protected/cloud/$organizationSlug/-components/store-teardown-section.tsx": "deleting an Environment or Project is destructive: it reads what goes before the user confirms, then waits on each removal Deployment",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useStoreChangeActions.tsx": "deploying starts runtime work and opens the admitted Deployment, and a Deploy or Publish that deletes Volume data asks the user first (Publish itself shows at once)",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreDeploymentPage.tsx": "retry starts runtime work and opens the new Deployment",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/pr-environments/StorePrPlanPanel.tsx": "a new Start from shows at once; the panel watches its answer only to move back over the canvas it was on when refused",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel.tsx": "the page opens a new Branch's canvas once the Store has it and Cloud knows its route",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useConfigCreator.ts": "creates the canvas node at once, then opens its files once the Store can serve the new Config",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/StoreConfigDrawer.tsx": "saves optimistically and retains submitted file drafts through queued refreshes until their own save succeeds",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useServiceCreator.ts": "hands a new Project's Service save to service-create-command, whose next page can't show it before the Store has it",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useDeleteService.ts": "a Database Preset's removal shows at once; its Volume's data goes only once the Store accepts the Service's removal",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/branch-close.tsx": "closing a Branch is destructive: it waits for its removal, which may ask before deleting Volume data",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/SyncDialog.tsx": "the page opens the receiver once it holds the synced changes, and a stale review keeps the dialog open with the fresh rows",
};

function walk(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? walk(path) : [path];
  });
}

const sources = walk(SRC)
  .map((path) => relative(SRC, path))
  .filter((path) => /\.tsx?$/.test(path) && !/\.test\.tsx?$/.test(path) && path !== "routeTree.gen.ts")
  .map((path) => ({ path, text: readFileSync(join(SRC, path), "utf8") }));

function filesMatching(pattern: RegExp) {
  return sources.filter(({ text }) => pattern.test(text)).map(({ path }) => path).sort();
}

describe("data boundaries", () => {
  it("creates every data source in a registered data file", () => {
    expect(filesMatching(CREATES_SOURCE), "Register the source in collections/data-sources.ts").toEqual(Object.keys(dataSources).sort());
    expect(Object.keys(dataSources).filter((path) => !DATA_FILE.test(path)), "Name data files *.collection.ts, *.queries.ts, or *.stream.ts").toEqual([]);
  });

  it("derives views in live queries, never as collections", () => {
    expect(filesMatching(/\b(liveQueryCollectionOptions|createLiveQueryCollection)\b/), "Shape rows in the source's queryFn, or query it with useLiveQuery").toEqual([]);
  });

  it("renders one shell, gated once on the Org Store", () => {
    expect(filesMatching(/<DashboardShell\b/)).toEqual(["routes/_protected/cloud/$organizationSlug/route.tsx"]);
    expect(filesMatching(/\buseOrgStoreGate\(/).filter((path) => path !== "collections/org-store.ts")).toEqual(["components/dashboard-shell.tsx"]);
  });

  it("writes to the Config Store only through its writer", () => {
    expect(filesMatching(/\bwriteStoreServerFn\b/), "Write through useStoreWriter").toEqual([
      "modules/config-store/store-write.ts", "modules/config-store/store.functions.ts",
    ]);
  });

  it("reads page state only through data files", () => {
    const outside = filesMatching(READ_SERVER_FN).filter((path) => !DATA_FILE.test(path) && !SERVER_FN_FILE.test(path));
    expect(outside, "Move the read into a data file, or list a command-step read with its reason").toEqual(Object.keys(COMMAND_READ_FILES).sort());
  });

  it("touches the network only from data files and server code", () => {
    const outside = filesMatching(NETWORK).filter((path) => !DATA_FILE.test(path) && !SERVER_FN_FILE.test(path) && !path.startsWith("routes/api/"));
    expect(outside, "Move the request into a data file").toEqual(Object.keys(NETWORK_FILES).sort());
  });

  it("prefetches every Remote Read in a loader unless it is read on demand", () => {
    const remoteFiles = Object.entries(dataSources).filter(([path, source]) => source.kind === "remote" && path.endsWith(".queries.ts")).map(([path]) => path);
    const factories = remoteFiles.flatMap((path) => [...readFileSync(join(SRC, path), "utf8").matchAll(/export function (\w+Options)\(/g)].map((match) => match[1] ?? ""));
    const loaderCode = sources.filter(({ path }) => path.startsWith("routes/") || path === "collections/route-data.ts").map(({ text }) => text).join("\n");
    // Presence check: some loader (or a route-data helper) prefetches the factory; review checks it is the page's own loader.
    const unprefetched = factories.filter((name) => !new RegExp(`(prefetchRemote\\w*\\([^;]*?|ensureQueryData\\()\\b${name}\\(`).test(loaderCode));
    expect(unprefetched.sort(), "Prefetch it with prefetchRemote in the page's loader, or list it as on demand").toEqual(Object.keys(ON_DEMAND_READS).sort());
  });

  it("uses spinners only for writes and running processes", () => {
    expect(filesMatching(SPINNER), "Reads use prefetched content, a skeleton, or nothing").toEqual(Object.keys(SPINNER_FILES).sort());
  });

  it("keeps loaders to route decisions and prefetches, and gives every Query read a freshness", () => {
    const api = new API({ cwd: root });
    const configPath = `${root}/tsconfig.json`;
    try {
      const snapshot = api.updateSnapshot({ openProject: configPath });
      try {
        const project = snapshot.getProject(configPath);
        if (!project) throw new Error(`TypeScript project not found: ${configPath}`);
        const violations: string[] = [];
        const awaitingUi = new Set<string>();
        const at = (source: SourceFile, node: Node, message: string) => {
          const line = source.text.slice(0, node.pos).split("\n").length;
          violations.push(`${relative(SRC, source.fileName)}:${line} ${message}`);
        };
        /** `x.isPersisted`, or `isPersisted` destructured from a write. */
        const isPersistedRef = (node: Node) => (isPropertyAccessExpression(node) && node.name.text === "isPersisted")
          || (isIdentifier(node) && node.text === "isPersisted");
        /** `x.isPersisted.promise`, directly or under `.catch(...)` and the like. */
        const persistenceChain = (node: Node): boolean => isPropertyAccessExpression(node)
          ? (node.name.text === "promise" && isPersistedRef(node.expression)) || persistenceChain(node.expression)
          : isCallExpression(node) && persistenceChain(node.expression);
        const calleeName = (node: Node) => {
          if (!isCallExpression(node)) return null;
          if (isIdentifier(node.expression)) return node.expression.text;
          if (isPropertyAccessExpression(node.expression)) return node.expression.name.text;
          return null;
        };

        for (const file of project.rootFiles) {
          if (!file.startsWith(`${SRC}/`) || file.endsWith(".d.ts") || /\.test\.tsx?$/.test(file)) continue;
          const source = project.program.getSourceFile(file);
          if (!source) continue;
          const isRoute = file.startsWith(`${SRC}/routes/`);
          // Only Org Store tables in collections/ inherit the createApiCollection staleTime default.
          const isCollectionsFile = file.startsWith(`${SRC}/collections/`);
          // Command hooks elsewhere are UI too: components wait through them. Data files only read.
          const path = relative(SRC, file);
          const isHookFile = /\bexport (function|const) use[A-Z]/.test(source.text) && !DATA_FILE.test(path) && !(path in HOOK_FILES_NOT_COMMANDS);
          const isUi = isRoute || file.startsWith(`${SRC}/components/`) || isHookFile;
          const serverCalls = new Set<string>();
          const importedFrom = new Map<string, string>();
          for (const statement of source.statements) {
            if (!isImportDeclaration(statement) || !isStringLiteral(statement.moduleSpecifier)) continue;
            const bindings = statement.importClause?.namedBindings;
            if (bindings && isNamedImports(bindings)) {
              for (const element of bindings.elements) importedFrom.set(element.name.text, statement.moduleSpecifier.text);
            }
          }

          const checkLoader = (body: Node) => {
            body.forEachChild(function inLoader(inner) {
              if (isAwaitExpression(inner)) {
                const name = calleeName(inner.expression);
                const from = name ? importedFrom.get(name) : undefined;
                // The session read is isomorphic: the client reads it from memory.
                const allowed = (name && /^(require|prefetch)[A-Z]/.test(name) && from?.endsWith("collections/route-data"))
                  || (name === "getAuthSession" && from?.endsWith("auth/auth"));
                if (!allowed) at(source, inner, "loaders may await only require*/prefetch* helpers from #/collections/route-data");
              }
              const name = calleeName(inner);
              if (name && (/ServerFn$/.test(name) || ["ensureQueryData", "prefetchQuery", "fetchQuery", "preload", "preloadCollection"].includes(name))) {
                at(source, inner, `loaders must not call ${name}; add a require*/prefetch* helper`);
              }
              inner.forEachChild(inLoader);
            });
          };
          const isLoaderName = (name: Node) => isIdentifier(name) && ["loader", "beforeLoad"].includes(name.text);
          const property = (options: Node, name: string) => isObjectLiteralExpression(options)
            ? options.properties.find((entry) => isPropertyAssignment(entry) && isIdentifier(entry.name) && entry.name.text === name)
            : undefined;

          source.forEachChild(function visit(node) {
            if (isRoute && isPropertyAssignment(node) && isLoaderName(node.name)) {
              if (isIdentifier(node.initializer)) at(source, node, "write loaders inline so their awaits can be checked");
              else checkLoader(node.initializer);
            }
            if (isRoute && isMethodDeclaration(node) && isLoaderName(node.name) && node.body) checkLoader(node.body);
            if (isRoute && isShorthandPropertyAssignment(node) && isLoaderName(node.name)) at(source, node, "write loaders inline so their awaits can be checked");
            if (isVariableDeclaration(node) && isIdentifier(node.name) && node.initializer && calleeName(node.initializer) === "useServerFn") {
              serverCalls.add(node.name.text);
            }
            if (isUi && isAwaitExpression(node)) {
              const awaited = node.expression;
              const awaitedName = calleeName(awaited);
              const persistence = isPropertyAccessExpression(awaited) && awaited.name.text === "promise" && isPersistedRef(awaited.expression);
              if (persistence || (awaitedName && (/ServerFn$/.test(awaitedName) || serverCalls.has(awaitedName)))) {
                awaitingUi.add(relative(SRC, source.fileName));
              }
            }
            // A save's promise handed on (to a helper that awaits it) waits for it too; `.catch` only rolls back.
            if (isUi && isPropertyAccessExpression(node) && node.name.text === "promise" && isPersistedRef(node.expression)
              && !(isPropertyAccessExpression(node.parent) && node.parent.name.text === "catch")) {
              awaitingUi.add(relative(SRC, source.fileName));
            }
            // Chaining .then or .finally on a save waits for it just as `await` does.
            if (isUi && isCallExpression(node) && isPropertyAccessExpression(node.expression)
              && ["then", "finally"].includes(node.expression.name.text) && persistenceChain(node.expression.expression)) {
              awaitingUi.add(relative(SRC, source.fileName));
            }
            const name = calleeName(node);
            if (name && isCallExpression(node)) {
              const options = node.arguments[0];
              if (["queryOptions", "infiniteQueryOptions"].includes(name) && !(options && property(options, "staleTime"))) {
                at(source, node, `${name} must declare staleTime in its options literal`);
              }
            }
            const queryFn = isObjectLiteralExpression(node) ? property(node, "queryFn") : undefined;
            const fetches = queryFn && isPropertyAssignment(queryFn) && !(isIdentifier(queryFn.initializer) && queryFn.initializer.text === "skipToken");
            if (fetches && !isCollectionsFile && !property(node, "staleTime")) {
              at(source, node, "options with a queryFn must declare staleTime");
            }
            node.forEachChild(visit);
          });
        }
        expect(violations).toEqual([]);
        expect([...awaitingUi].sort(), "Make the write optimistic, or list a command with its reason").toEqual(Object.keys(COMMAND_FILES).sort());
      } finally {
        snapshot.dispose();
      }
    } finally {
      api.close();
    }
  });
});
