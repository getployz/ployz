import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import { previewComment, type PreviewLink } from "./store-pr-preview";
import { previewInCheck } from "./store-pr-preview-check.server";
import { PR_CHECK_NAME } from "./store-pull-requests";

const APP = "https://cloud.ployz.test/";
const HEAD = "a".repeat(40);
const CONFIRM = "https://cloud.ployz.test/cloud/acme/shop/previews/42/3";
const pull = { repositoryId: 42, number: 3 };

/** A Store's view of pull request 3 listing `previews`, open unless `open` says otherwise. */
const view = (previews: PreviewLink[], open = true) => ({ pull_request: { open }, previews });

const say = (previews: PreviewLink[], open = true) => previewComment([{ slug: "acme", view: view(previews, open) }], pull, APP);
const link = (outcome: PreviewLink["outcome"], project = "shop"): PreviewLink => ({ project, outcome });

describe("previewComment", () => {
  it("offers the confirm page until a preview is live, then opens it", () => {
    expect(say([link({ state: "not_deployed" })])?.line).toBe(`[Deploy as a preview](${CONFIRM})`);
    expect(say([link({ state: "not_deployed", why: { kind: "full" } })])?.line).toBe(`[Deploy as a preview](${CONFIRM})`);
    expect(say([link({ state: "live", address: "https://web.pr-3.acme.ployz.app" })])?.line)
      .toBe("[Open preview](https://web.pr-3.acme.ployz.app/) https://web.pr-3.acme.ployz.app/");
  });

  it("asks again for a fork's newer commits, from the confirm page", () => {
    expect(say([link({ state: "live", address: "web.pr-3.acme.ployz.app", newer: "b".repeat(40) })])?.line)
      .toBe(`[Open preview](https://web.pr-3.acme.ployz.app/) https://web.pr-3.acme.ployz.app/ · New commits: [deploy again](${CONFIRM})`);
  });

  it("says nothing without previews, and takes the comment down once they're off everywhere", () => {
    expect(previewComment([{ slug: "acme", view: { pull_request: { open: true } } }], pull, APP)).toBeNull();
    expect(say([link({ state: "not_deployed", why: { kind: "off" } })])).toEqual({ calls: [], line: null });
  });

  it("never offers a closed pull request a preview", () => {
    expect(say([link({ state: "not_deployed" })], false)?.line).toBeNull();
    expect(say([link({ state: "live", address: "https://web.pr-3.acme.ployz.app" })], false)?.line).toBe("Preview closed");
  });

  it("names each Project when several preview the repository", () => {
    expect(say([link({ state: "not_deployed" }), link({ state: "not_deployed" }, "blog")])?.line)
      .toBe(`shop: [Deploy as a preview](${CONFIRM}) · blog: [Deploy as a preview](https://cloud.ployz.test/cloud/acme/blog/previews/42/3)`);
  });
});

describe("previewInCheck", () => {
  const calls = say([link({ state: "not_deployed" })])?.calls ?? [];

  it("posts the link as a neutral check, its title and Details", () => {
    expect(Effect.runSync(previewInCheck(7, calls, HEAD))).toEqual({
      headSha: HEAD, name: PR_CHECK_NAME, conclusion: "neutral", title: "Deploy as a preview", detailsUrl: CONFIRM,
      summary: `[Deploy as a preview](${CONFIRM})`,
    });
  });
});
