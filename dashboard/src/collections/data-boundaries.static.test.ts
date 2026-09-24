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
} from "typescript/unstable/ast/is";
import { dataSources } from "./data-sources";

const root = process.cwd();
const SRC = join(root, "src");
const DATA_FILE = /^collections\/[^/]+\.ts$|\.collection\.ts$|\.queries\.ts$|\.stream\.ts$/;
const CREATES_SOURCE = /\b(createApiCollection|queryCollectionOptions|localOnlyCollectionOptions|liveQueryCollectionOptions|queryOptions|infiniteQueryOptions|createCollection)\s*[<(]|\bqueryFn\s*:/;
const SPINNER = /<Spinner\b|Loader2Icon|animate-spin/;
/** Spinners mean a write is in flight or a runtime process is running, never a read. */
const SPINNER_FILES = {
  "components/ui/spinner.tsx": "the primitive",
  "components/ui/sonner.tsx": "promise toasts for writes",
  "components/cancel-deployment-dialog.tsx": "cancel in flight",
  "components/confirm-destructive-dialog.tsx": "confirm in flight",
  "components/confirm-dialog.tsx": "confirm in flight",
  "components/data-loss/data-loss-confirm-dialog.tsx": "submit in flight",
  "components/destructive-volume/volume-destruction-confirmation-dialog.tsx": "submit in flight",
  "components/deployment-logs.tsx": "deployment step running",
  "components/deployment-status-card.tsx": "deployment running",
  "components/navigation-switcher.tsx": "create in flight",
  "components/service-create-command.tsx": "create in flight",
  "components/service-source-selector.tsx": "sync and submit in flight",
  "components/stageable/confirmable-input.tsx": "save in flight",
  "components/variables/variable-add-form.tsx": "submit in flight",
  "form/index.tsx": "submit in flight",
  "routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section.tsx": "retry in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/BillingPlanCard.tsx": "checkout in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/BillingPlanChangeDialog.tsx": "plan change in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection.tsx": "reset in flight",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/add-server-dialog.tsx": "command mint in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/CanvasInspectorNameEditor.tsx": "rename in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/VariableGroupCreatorDialog.tsx": "create in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/VolumeCreatorDialog.tsx": "create in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeDrawer.tsx": "retry in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariableGroupAttachmentsPanel.tsx": "attach in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorFooter.tsx": "save in flight",
  "routes/_public/-components/AppHeaderActions.tsx": "sign-out in flight",
  "routes/_public/-components/LoginDialog.tsx": "sign-in in flight",
};

const READ_SERVER_FN = /\b(get|load|list|preview|search|resolve|read)[A-Z]\w*ServerFn\b/;
const SERVER_FN_FILE = /[.-]functions\.ts$|\.server\.ts$/;
/** Reads that are one step of a user command (preview, evidence, wait for completion), not page state. */
const COMMAND_READ_FILES = {
  "components/service-source-selector.tsx": "resolve a pasted public repository before connecting it",
  "routes/_protected/cloud/$organizationSlug/-components/teardown-danger-section.tsx": "gather data-loss evidence before confirming teardown",
  "routes/_protected/cloud/$organizationSlug/_org/~/billing.tsx": "preview a plan change before confirming it",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/server-list-rows.tsx": "gather data-loss evidence, then wait for the confirmed removal",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/resources/$resourceId/-components/VolumeDrawer.tsx": "gather data-loss evidence before confirming removal",
};

const NETWORK = /\bfetch\(|new EventSource\(/;
/** Raw network access outside data files and server code. */
const NETWORK_FILES = {
  "modules/github/github-observation.api.ts": "server-only GitHub API client",
  "providers/runtime-provider.tsx": "the Runtime SSE connection",
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

  it("renders one shell, gated once on the Org Store", () => {
    expect(filesMatching(/<DashboardShell\b/)).toEqual(["routes/_protected/cloud/$organizationSlug/route.tsx"]);
    expect(filesMatching(/\buseOrgStoreGate\(/).filter((path) => path !== "collections/org-store.ts")).toEqual(["components/dashboard-shell.tsx"]);
  });

  it("reads page state only through data files", () => {
    const outside = filesMatching(READ_SERVER_FN).filter((path) => !DATA_FILE.test(path) && !SERVER_FN_FILE.test(path));
    expect(outside, "Move the read into a data file, or list a command-step read with its reason").toEqual(Object.keys(COMMAND_READ_FILES).sort());
  });

  it("touches the network only from data files and server code", () => {
    const outside = filesMatching(NETWORK).filter((path) => !DATA_FILE.test(path) && !SERVER_FN_FILE.test(path) && !path.startsWith("routes/api/"));
    expect(outside, "Move the request into a data file").toEqual(Object.keys(NETWORK_FILES).sort());
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
        const at = (source: SourceFile, node: Node, message: string) => {
          const line = source.text.slice(0, node.pos).split("\n").length;
          violations.push(`${relative(SRC, source.fileName)}:${line} ${message}`);
        };
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
          // Only Org Store tables in collections/ inherit the createApiCollection default.
          const isCollectionsFile = file.startsWith(`${SRC}/collections/`);
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
      } finally {
        snapshot.dispose();
      }
    } finally {
      api.close();
    }
  });
});
