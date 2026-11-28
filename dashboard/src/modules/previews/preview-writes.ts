/**
 * A preview's Deploy and Close, sent as the Store's own commands through the Config Store's writer. Neither shows
 * before the Store answers: a Deploy is refused at the limit or for a newer commit, and a Close deletes the preview's
 * data, so the page waits and shows what the Store says. The Store words each refusal as the page shows it.
 */
import type { ClosePreview, DeployPullRequestPreview } from "@ployz/sdk";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { StoreRefused } from "#/lib/refusal";
import { invalidateViews } from "#/modules/config-store/store-view.queries";
import { showRefusalInline, useStoreWriter } from "#/modules/config-store/store-write";

export function usePreviewWrites(organizationSlug: string) {
  const { queryClient } = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  async function send(command: ({ command: "deploy_preview" } & DeployPullRequestPreview) | ({ command: "close_preview" } & ClosePreview)) {
    const write = writer.commit(command);
    showRefusalInline(write);
    const written = await write;
    // A preview is a PR Environment: the previews, the Project's Environments and the pull request all move.
    void invalidateViews(queryClient, organizationSlug, null);
    return written;
  }
  return {
    deploy: (pull: DeployPullRequestPreview) => send({ command: "deploy_preview", ...pull }),
    close: (preview: ClosePreview) => send({ command: "close_preview", ...preview }),
  };
}

/** What a refused or failed preview write says, as the page shows it. */
export function writeFailure<T>(error: T) {
  if (error instanceof StoreRefused && error.message) return error.message;
  return error instanceof TypeError ? "Couldn't reach Ployz Cloud. Try again." : "That didn't work. Try again.";
}
