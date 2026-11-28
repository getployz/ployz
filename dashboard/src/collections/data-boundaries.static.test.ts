import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";
import { API, SignatureKind, type Type } from "typescript/unstable/sync";
import type { Node, SourceFile } from "typescript/unstable/ast";
import {
  isAwaitExpression,
  isCallExpression,
  isIdentifier,
  isImportDeclaration,
  isInterfaceDeclaration,
  isMethodDeclaration,
  isNamedImports,
  isObjectLiteralExpression,
  isPropertyAccessExpression,
  isPropertyAssignment,
  isShorthandPropertyAssignment,
  isStringLiteral,
  isVariableDeclaration,
} from "typescript/unstable/ast/is";

const root = process.cwd();
const SRC = join(root, "src");
const DATA_FILE =/^collections\/[^/]+\.ts$|\.collection\.ts$|\.queries\.ts$|\.stream\.ts$/;
const CREATES_SOURCE = /\b(createApiCollection|createChangeCollection|queryCollectionOptions|localOnlyCollectionOptions|liveQueryCollectionOptions|queryOptions|infiniteQueryOptions|createCollection)\s*[<(]|\bqueryFn\s*:/;
// The first line of a data file: /** @data-source <kind>: <freshness> */
const DATA_SOURCE_HEADER = /^\/\*\* @data-source (\S+?): (.+) \*\/$/;
const DATA_SOURCE_KINDS = ["org-store", "runtime", "remote"];
const ROUTE_DATA_FILE = /(^|[./])route-data\.ts$/;
const ROUTE_DATA_MODULE = /(^|[./])route-data$/;
const SPINNER = /<Spinner\b|Loader2Icon|animate-spin/;
/** Spinners mean a write is in flight or a runtime process is running, never a read. */
const SPINNER_FILES = {
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-automatic-deploys.tsx": "a rule's passkey prompt and its save in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId/-components/StoreVolumeCopies.tsx": "a Volume run under way or a requested Refresh now",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel.tsx": "creating a Branch in the Config Store",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/previews/PreviewPage.tsx": "a preview's Deploy in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/previews/ClosePreviewPage.tsx": "a Close, then the Deploy it made room for, in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/previews/PreviewSettingsPage.tsx": "the preview rule's passkey prompt and its save in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/MirrorNode.tsx": "a copy being made or refreshed",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/VolumeTray.tsx": "a Volume run under way",
  "components/ui/spinner.tsx": "the primitive",
  "components/agent-sidebar/agent-panel.tsx": "the agent starting its reply",
  "components/agent-sidebar/tool-row.tsx": "a command the agent is running",
  "components/ui/sonner.tsx": "promise toasts for writes",
  "components/confirm-dialog.tsx": "confirm in flight",
  "components/deletion-dialog.tsx": "deletion in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/SyncDialog.tsx": "sync in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentHistory.tsx": "a Restore waiting on the Store",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/BottomBar.tsx": "a Save or Deploy waiting on the Store",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/EnvironmentChangesReview.tsx": "a Save or Deploy waiting on the Store",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/SyncButton.tsx": "a Sync waiting on the Store",
  "components/shell/create-environment-dialog.tsx": "create in flight",
  "components/shell/new-organization-dialog.tsx": "create in flight",
  "routes/_protected/cloud/$organizationSlug/_project/-components/service-create/service-create-command.tsx": "create in flight",
  "routes/_protected/cloud/$organizationSlug/_project/-components/service-create/service-source-selector.tsx": "sync and submit in flight",
  "form/index.tsx": "submit in flight",
  "components/settings/settings-list.tsx": "a settings form's Save in flight",
  "components/store-write/confirmable-input.tsx": "a confirm row's save in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-teardown-section.tsx": "removal in flight",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId/-components/domain-row.tsx": "Setting up while the Cluster Domain sync runs; Issuing certificate while the certificate is ordered",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId/-components/ServiceActions.tsx": "a restart, stop or start moving the Service's containers",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId/-components/monitoring/MonitoringTab.tsx": "Checking while a requested probe runs",
  "routes/_protected/cloud/$organizationSlug/_org/-components/ClusterDomainSection.tsx": "Setting up while the sync runs; Check again until the sync lands",
  "routes/_protected/cloud/$organizationSlug/_org/-components/WebTrafficSection.tsx": "a switch running on the Servers; Find zones and Set up in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/PendingEnrollmentResetSection.tsx": "reset in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/integrations.tsx": "Connect starting a router on every Server",
  "routes/_protected/cloud/$organizationSlug/_org/-components/members-settings.tsx": "invite, remove or leave in flight",
  "routes/_protected/cloud/$organizationSlug/_org/-components/tokens-settings.tsx": "token mint in flight",
  "routes/invite/$token.tsx": "joining in flight",
  "routes/claim/$secret.tsx": "keeping the Organization in flight",
  "routes/_protected/cloud/$organizationSlug/_org/~/billing.tsx": "checkout or portal opening",
  "modules/machines/add-server/install-command.tsx": "token mint in flight, then waiting for the first server; Find zones and Check in flight",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/first-tunnel.tsx": "the tunnel setting up on the first server",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/server-services-section.tsx": "the Drain running on the Server",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/server-web-traffic-section.tsx": "the web server starting or stopping on the Server",
  "routes/_public/-components/LoginPanel.tsx": "sign-in in flight",
  "routes/device.tsx": "device approval in flight",
  "routes/connect.tsx": "consent answer in flight",
};

const READ_SERVER_FN = /\b(get|load|list|preview|search|resolve|read)[A-Z]\w*ServerFn\b/;
const SERVER_FN_FILE = /[.-]functions\.ts$|\.server\.ts$/;
/** Reads that are one step of a user command (preview, evidence, wait for completion), not page state. */
const COMMAND_READ_FILES = {
  "routes/_protected/cloud/$organizationSlug/_project/-components/service-create/service-source-selector.tsx": "resolve a pasted public repository before connecting it",
  "modules/machines/add-server/install-command.tsx": "find the zones a pasted Cloudflare token reaches before founding closed; the token stays in the tab and is never stored",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/remove-server-section.tsx": "gather data-loss evidence, then wait for the confirmed removal",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/stray-namespaces.tsx": "gather a Namespace's data-loss evidence before the user confirms its removal",
  "routes/_protected/cloud/$organizationSlug/_org/-components/forget-servers-dialog.tsx": "try every Server and list what goes before the user confirms forgetting them",
  "routes/_protected/cloud/$organizationSlug/_org/-components/WebTrafficSection.tsx": "find the zones a pasted Cloudflare token reaches before setting up; the token is used once and never stored",
};

const NETWORK = /\bfetch\(|new EventSource\(/;
/** Raw network access outside data files and server code. */
const NETWORK_FILES = {
  "lib/server-fn-fetch.ts": "the transport under every server function call from the browser",
  "modules/github/github-observation.api.ts": "server-only GitHub API client",
  "modules/inngest/client.ts": "server-only Inngest REST lookup of one run's status for the volume run sweep",
};

/** Remote Reads a loader cannot prefetch, and what warms them instead. */
const ON_DEMAND_READS = {
  logHistoryStatusQueryOptions: "the Logs view reads it once mounted and polls it, and a log line never waits on it",
  logHistoryServicesQueryOptions: "the Logs view reads it once the Environment's history is on, and the picker shows the loaded Services until it lands",
  machineRemoveAttemptQueryOptions: "named by the attempt the user's confirmed Remove or Forget just queued, known only once the Servers take it",
  clusterFoundedQueryOptions: "read only while this tab holds a Cloudflare token for the first Server, which runs the tunnel setup once it founds",
  githubFileSearchQueryOptions: "searches as the user types",
  githubRepoAccessQueryOptions: "read when a repository picker opens; installation access changes in GitHub, so each picker rechecks it",
  githubBranchesQueryOptions: "depends on the repository the user just picked",
  githubRepositoryOwnerQueryOptions: "read when the automatic deploys dialog opens, for the repository the drafted rule names",
  githubBuildRepositoriesQueryOptions: "calls GitHub per repository; Organization Settings › Builds fills it in after hydration",
  volumeMembersQueryOptions: "asks every Server for its copy live, so a slow Server would hold the drawer open; the Placement section waits on it instead",
  strayNamespacesQueryOptions: "asks about the Namespaces the Servers report live, known only once the Runtime Watch answers",
  serviceMarkersQueryOptions: "one per canvas card, asking every Server live; the card shows nothing extra until it answers, so the canvas never waits on a slow Server",
  pendingApprovalsOptions: "polled by the agent sidebar, which every Organization page mounts; no page owns it",
  pendingJoinsQueryOptions: "asks every Server live for joins waiting on a passkey, so a slow Server would hold the Servers page; the list appears once they answer",
  installKeyQueryOptions: "asks the Servers live which passkey they trust, read fresh each time an install command is shown",
  agentThreadsOptions: "read when the agent opens, which every Organization page can do; no page owns it",
  approvalOptions: "named by an agent conversation's interrupt, known only once the chat hydrates",
  pendingProposalsOptions: "polled by the agent sidebar, which every Organization page mounts; no page owns it",
  proposalOptions: "a card's own proposal, named by the waiting list or this browser's Recent list",
  serviceMetricsQueryOptions: "asks every Server live, so a slow Server would hold the drawer open; the Monitoring tab waits on it instead",
  serverMetricsQueryOptions: "asks a Server live, so a slow Server would hold the page; each Servers list row reads its own Server after the list shows, and the Resources section waits on it",
  latestServiceActQueryOptions: "keyed by the Service's name, which the drawer knows only once it reads the Store; the header shows no run until it answers",
  runtimeServiceMetricsQueryOptions:"asks every Server live, so a slow Server would hold the page; each Running here row waits on its own",
  clusterIngressObservationQueryOptions: "asks every Server live for the doors it holds, so a slow Cluster never holds Settings; the section shows the stored state until it answers",
  privateAccessQueryOptions: "asks every Server live for its router, so a slow Cluster never holds Integrations; the section says it is checking until it answers",
  lastAnswerOptions: "fetches nothing: it holds the Servers' last answer to a Connect or Turn off made in this tab",
  reviewDoorsSwitchQueryOptions: "the switch dialog's own review of what moves, read when it opens",
};

/** Hook files outside data files that await the server without making UI wait on it. */
const HOOK_FILES_NOT_COMMANDS = {
  "modules/config-store/store-write.ts": "the Store writer owns the per-Environment optimistic queue; edits show before they save, and UI awaiting a commit is listed itself",
};

/** UI that waits for the server, and why. Everything else applies writes optimistically. */
const COMMAND_FILES = {
  "modules/previews/preview-writes.ts": "a preview's Deploy is refused at the limit or for a newer commit, and a Close deletes the preview's data, so the page shows only what the Store answers",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/previews/PreviewSettingsPage.tsx": "turning previews on or changing where they start signs the preview rule with a passkey first; the sheet stays open to show a refusal and sign again",
  "modules/deploy-approval/approve-deploy.tsx": "an approval page plans the Deploy again and waits on the passkey prompt before it decides; what shows next is the Store's and the Servers' answer, which can't be guessed",
  "modules/deploy-approval/deploy-sheet.tsx": "a Deploy its Servers run only signed waits for its own passkey prompt before it is sent; the sheet stays open to show a refusal and sign again, and a caller opens the Deployment only once the Store has it",
  "modules/deploy-approval/save-sheet.tsx": "a Save that signs the automatic deploy rule waits for its own passkey prompt before it is sent; the sheet stays open to show a refusal and sign again, and a Volume-loss retry carries the same answer",
  "modules/machines/server-regions.hooks.ts": "the Store refuses an unknown region or another region's name, and the rename dialog stays open to show it; the cached names change only once the Store has taken one",
  "routes/_protected/cloud/$organizationSlug/_org/-components/passkeys-section.tsx": "adding a passkey waits on the browser's authenticator and on the server, which assigns its code",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-automatic-deploys.tsx": "a rule is signed by its own passkey prompt before it can be saved, so nothing shows before it is; the dialog stays open to show a refusal, such as a newer rule another tab approved",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/EnvironmentHistory.tsx": "the Store derives a sealed History inverse and exact overwrite review before staging; it cannot be guessed optimistically",
  "routes/_protected/cloud/$organizationSlug/_project/-components/service-create/service-create-command.tsx": "the server assigns a new project's id and slug, and its canvas can't show the new Service before the Store has it",
  "components/store-write/use-commit-hold.ts": "a committed control keeps showing the value it last wrote until that write settles; the save only releases that hold, and nothing waits on it",
  "routes/_protected/cloud/$organizationSlug/_project/-components/service-create/service-source-selector.tsx": "resolving a public repository and syncing GitHub are external",
  "routes/_protected/cloud/$organizationSlug/_org/~/billing.tsx": "checkout and the billing portal involve money",
  "modules/billing/use-embedded-checkout.ts": "checkout involves money",
  "routes/_protected/cloud/$organizationSlug/_org/-components/github-settings.tsx": "linking a GitHub account checks the signed offer against GitHub, and a disconnect answers with the account's uninstall link",
  "routes/_protected/cloud/$organizationSlug/_org/-components/store-organization-danger.tsx": "deleting an organization is destructive and waits on its Servers letting go",
  "routes/_protected/cloud/$organizationSlug/_org/-components/forget-servers-dialog.tsx": "forgetting the Servers is destructive: it tries every Server again and forgets only when none answers",
  "routes/_protected/cloud/$organizationSlug/_org/-components/WebTrafficSection.tsx": "setting up creates a tunnel in Cloudflare with a token used once, and says what Cloudflare kept if it stops; a switch moves every address and runs on the Servers, so the dialog waits for Cloud to admit it",
  "routes/_protected/cloud/$organizationSlug/_org/-components/integrations.tsx": "Connect and Turn off run on every Server, and the card lists each Server that refused the key, so it waits for their answers",
  "modules/volume-placement/volume-placement.hooks.ts": "a Deploy's Volume workflows run only with answers the owner's passkey gives, so the Deploy waits for them to be kept before it starts",
  "routes/_protected/cloud/index.tsx": "the server creates the new organization and its slug, which the page then opens",
  "routes/connect.tsx": "the answer sends the browser to the agent's callback, a URL only the server builds",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-settings.tsx": "renaming a Project changes its URLs: the page opens the new one once the Store has it",
  "components/shell/create-environment-dialog.tsx": "over the Store the dialog stays open until the name is accepted, then opens the new environment",
  "modules/machines/add-server/install-command.tsx": "checking a Cloudflare zone reaches Cloudflare with a token used once, and the install line waits for the founding choices to be accepted",
  "modules/ingress/cloudflare-setup.ts": "setting up the tunnel creates it in Cloudflare and runs on the first Server, so the card waits for the result",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/first-tunnel.tsx": "setting up the tunnel creates it in Cloudflare and runs on the first Server, so the card waits for the result",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/remove-server-section.tsx": "removing a server is destructive and waits on the runtime",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/server-trash-section.tsx": "a Trash restore or empty waits on the person's passkey and then on the one Server, which checks the approval before anything changes",
  "routes/_protected/cloud/$organizationSlug/_org/~/servers/-components/stray-namespaces.tsx": "removing a Namespace no Project owns deletes its Volumes' data: it reads what goes before the user confirms, then waits on the Servers",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/store-teardown-section.tsx": "deleting an Environment or Project is destructive: it reads what goes before the user confirms, then shows what the Store still has to take off",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/StoreDeploymentPage.tsx": "retry starts runtime work and opens the new Deployment",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/new-branch/StoreNewBranchPanel.tsx": "the page opens a new Branch's canvas once the Store has it and Cloud knows its route",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/canvas/useConfigCreator.ts": "creates the canvas node at once, then opens its files once the Store can serve the new Config",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId/-components/StoreVolumeSize.tsx": "a grow is a Volume Run on the Servers, not a Store write; the row shows the Run's progress once Cloud admits it, and its refusal if it doesn't",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId/-components/MountEditor.tsx": "the mount form stays open until the Store accepts the mount, and shows its refusal under the field",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId/-components/StoreMounts.tsx": "an unmount shows Saving unmount until the Store answers, and a refused one shows why",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/resources/$resourceId/-components/StoreConfigDrawer.tsx": "saves optimistically and retains submitted file drafts through queued refreshes until their own save succeeds",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId/-components/useRemoveStoreService.ts": "a Database Preset's removal shows at once; its Volume's data goes only once the Store accepts the Service's removal",
  "routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/sync/branch-close.tsx": "closing a Branch is destructive: its removal may ask before deleting Volume data",
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
  .filter((path) => /\.tsx?$/.test(path) && !/\.test\.tsx?$/.test(path) && !path.startsWith("test/") && path !== "routeTree.gen.ts")
  .map((path) => ({ path, text: readFileSync(join(SRC, path), "utf8") }));

function filesMatching(pattern: RegExp) {
  return sources.filter(({ text }) => pattern.test(text)).map(({ path }) => path).sort();
}

describe("data boundaries", () => {
  it("creates every data source in a data file that declares its kind", () => {
    const declared = sources.flatMap(({ path, text }) => {
      const header = DATA_SOURCE_HEADER.exec(text.split("\n", 1)[0] ?? "");
      return header ? [{ path, kind: header[1] ?? "", freshness: header[2] ?? "" }] : [];
    });
    expect(filesMatching(CREATES_SOURCE), "Start the file with `/** @data-source <kind>: <freshness> */`").toEqual(declared.map(({ path }) => path).sort());
    expect(declared.filter(({ kind }) => !DATA_SOURCE_KINDS.includes(kind)).map(({ path }) => path), `The kind is one of ${DATA_SOURCE_KINDS.join(", ")}`).toEqual([]);
    expect(declared.filter(({ path }) => !DATA_FILE.test(path)).map(({ path }) => path), "Name data files *.collection.ts, *.queries.ts, or *.stream.ts").toEqual([]);
  });

  it("derives views in live queries, never as collections", () => {
    expect(filesMatching(/\b(liveQueryCollectionOptions|createLiveQueryCollection)\b/), "Shape rows in the source's queryFn, or query it with useLiveQuery").toEqual([]);
  });

  it("renders one shell, gated once on the Org Store", () => {
    expect(filesMatching(/<DashboardShell\b/)).toEqual(["routes/_protected/cloud/$organizationSlug/route.tsx"]);
    expect(filesMatching(/\buseOrgStoreGate\(/).filter((path) => path !== "collections/org-store.ts")).toEqual(["components/shell/dashboard-shell.tsx"]);
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
    const remoteFiles = sources.filter(({ path, text }) => path.endsWith(".queries.ts") && text.startsWith("/** @data-source remote:"));
    const factories = remoteFiles.flatMap(({ text }) => [...text.matchAll(/export function (\w+Options)\(/g)].map((match) => match[1] ?? ""));
    const loaderCode = sources.filter(({ path }) => path.startsWith("routes/") || ROUTE_DATA_FILE.test(path)).map(({ text }) => text).join("\n");
    // Presence check: some loader (or a route-data helper) prefetches the factory; review checks it is the page's own loader.
    const unprefetched = factories.filter((name) => !new RegExp(`((prefetchRemote\\w*|streamRemote\\w*)\\([^;]*?|ensureQueryData\\()\\b${name}\\(`).test(loaderCode));
    expect(unprefetched.sort(), "Prefetch it with prefetchRemote or streamRemote in the page's loader, or list it as on demand").toEqual(Object.keys(ON_DEMAND_READS).sort());
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
        const { checker } = project;
        const writeSource = project.program.getSourceFile(join(SRC, "modules/config-store/store-write.ts"));
        const writeDeclaration = writeSource?.statements.find((statement) => isInterfaceDeclaration(statement) && statement.name.text === "Write");
        const writeSymbol = writeDeclaration && isInterfaceDeclaration(writeDeclaration) ? checker.getSymbolAtLocation(writeDeclaration.name) : undefined;
        if (!writeSymbol) throw new Error("store-write.ts must declare Write as an interface");
        const named = (type: Type | undefined) => type?.getSymbol()?.id === writeSymbol.id;
        const beforeCatch = (node: Node): Node => isCallExpression(node) && isPropertyAccessExpression(node.expression)
          && node.expression.name.text === "catch" ? beforeCatch(node.expression.expression) : node;
        const isWrite = (node: Node) => named(checker.getTypeAtLocation(beforeCatch(node)));
        const makesWrite = (node: Node) => {
          const type = checker.getTypeAtLocation(node);
          return type !== undefined && checker.getSignaturesOfType(type, SignatureKind.Call).some((signature) => named(checker.getReturnTypeOfSignature(signature)));
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
          // Only Org Store tables in collections/ inherit the createApiCollection staleTime default.
          const isCollectionsFile = file.startsWith(`${SRC}/collections/`);
          // Command hooks elsewhere are UI too: components wait through them. Data files only read.
          const path = relative(SRC, file);
          const isHookFile = /\bexport (function|const) use[A-Z]/.test(source.text) && !DATA_FILE.test(path) && !(path in HOOK_FILES_NOT_COMMANDS);
          const isUi = isRoute || file.endsWith(".tsx") || isHookFile;
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
                const allowed = (name && /^(require|prefetch)[A-Z]/.test(name) && from !== undefined && ROUTE_DATA_MODULE.test(from))
                  || (name === "getAuthSession" && from?.endsWith("auth/auth"));
                if (!allowed) at(source, inner, "loaders may await only require*/prefetch* helpers from a *.route-data.ts file");
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
              const handsOnWrites = isCallExpression(awaited) && awaited.arguments.some(makesWrite);
              if (isWrite(awaited) || handsOnWrites || (awaitedName && (/ServerFn$/.test(awaitedName) || serverCalls.has(awaitedName)))) {
                awaitingUi.add(relative(SRC, source.fileName));
              }
            }
            // Chaining .then or .finally on a save waits for it just as `await` does.
            if (isUi && isCallExpression(node) && isPropertyAccessExpression(node.expression)
              && ["then", "finally"].includes(node.expression.name.text) && isWrite(node.expression.expression)) {
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
