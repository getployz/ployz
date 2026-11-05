import { Alert, AlertDescription } from "#/components/ui/alert";
import { GITHUB_REPOS_POLL_MS } from "#/modules/github/github.collection";

/** The open picker's poll reads again, so the notice says when rather than offering a Retry. */
const AGAIN = `Trying again every ${GITHUB_REPOS_POLL_MS / 1000} seconds.`;

/**
 * Why the picker has no repositories, or old ones. The poll still reads after a crash, but a crashed collection never
 * takes those reads. A new page starts it over at once; otherwise only its gcTime cleanup does, once no picker reads it.
 */
export function GithubRepositoryRefreshNotice({ initial = false, crashed = false }: { initial?: boolean; crashed?: boolean }) {
  return (
    <Alert>
      <AlertDescription>
        {crashed
          ? "Could not load repositories. Reload the page to try again."
          : initial
            ? `Could not load repositories. ${AGAIN}`
            : `Could not refresh repositories. Shown results may be out of date. ${AGAIN}`}
      </AlertDescription>
    </Alert>
  );
}
