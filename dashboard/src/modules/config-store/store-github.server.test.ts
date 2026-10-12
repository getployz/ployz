import { it } from "@effect/vitest";
import type { JsonValue } from "@ployz/sdk";
import { Effect } from "effect";
import { expect } from "vitest";
import { mergedInto } from "#/modules/config-store/store-github.server";
import { GithubApi } from "#/modules/github/github-observation.api";
import { fakeGithubApiBy } from "#/test/fake-github";

const MERGE = "3".repeat(40);
const LATER = "4".repeat(40);

/** GitHub with `acme/web` (42): `main` at LATER, which compares to MERGE as `status` says. */
function github(status: string) {
  return fakeGithubApiBy(({ url }): JsonValue => url.includes("/git/ref/")
    ? { ref: "refs/heads/main", object: { type: "commit", sha: LATER } }
    : url.includes(`/compare/${MERGE}...${LATER}`)
      ? { status, files: [] }
      : { id: 42, full_name: "acme/web", private: true });
}

const into = (status: string, mergeCommitSha: string | null) => {
  const fake = github(status);
  return mergedInto(7, 42, { mergeCommitSha, targetBranch: "main" }).pipe(
    Effect.provideService(GithubApi, fake.service),
    Effect.map((head) => ({ head, calls: fake.calls.length })),
  );
};

it.effect("a merge the branch moved past names the head that contains it", () =>
  Effect.gen(function* () {
    expect((yield* into("ahead", MERGE)).head).toBe(LATER);
    expect((yield* into("identical", MERGE)).head).toBe(LATER);
    // A head that doesn't contain the merge (a force-push dropped it) vouches for nothing.
    expect((yield* into("diverged", MERGE)).head).toBeNull();
    // Unmerged, GitHub isn't asked.
    expect(yield* into("ahead", null)).toEqual({ head: null, calls: 0 });
  }));
