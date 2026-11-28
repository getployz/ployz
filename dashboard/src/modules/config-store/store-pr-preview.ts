import { Option, Schema } from "effect";

const outcomeSchema = Schema.Union([
  Schema.Struct({ state: Schema.Literal("not_deployed"), why: Schema.optional(Schema.NullOr(Schema.Struct({ kind: Schema.String }))) }),
  Schema.Struct({ state: Schema.Literal("deploying") }),
  Schema.Struct({ state: Schema.Literal("live"), address: Schema.optional(Schema.NullOr(Schema.String)), newer: Schema.optional(Schema.NullOr(Schema.String)) }),
  Schema.Struct({ state: Schema.Literal("closed") }),
]);
const linkSchema = Schema.Struct({ project: Schema.String, outcome: outcomeSchema });

/** One Project's preview of a pull request, as `PullRequestView.previews` lists it. */
export type PreviewLink = typeof linkSchema.Type;

/** What the comment reads of a Store's `pull_request` view: whether the pull request is open, and its previews. */
export type PreviewsView = { readonly pull_request: { readonly open: boolean } | null; readonly previews?: unknown };

/**
 * The preview links a Store's view lists, each read on its own: a view without the field, or a link Cloud can't read,
 * says nothing.
 */
export function previewLinks(view: PreviewsView): PreviewLink[] {
  const listed = view.previews;
  if (!Array.isArray(listed)) return [];
  return listed.flatMap((link) => Option.toArray(Schema.decodeUnknownOption(linkSchema)(link)));
}

/** What the pull request's one comment says about one Project's preview: its words, where they lead, and as Markdown. */
export type PreviewCall = { text: string; url: string | null; markdown: string };

/** A live preview's address as a web URL, or null when the Store gave none Cloud can link. */
function addressUrl(address: string | null | undefined) {
  if (!address) return null;
  const url = URL.parse(/^[a-z][a-z0-9+.-]*:\/\//i.test(address) ? address : `https://${address}`);
  return url && (url.protocol === "https:" || url.protocol === "http:") ? url.href : null;
}

function call(link: PreviewLink, open: boolean, confirm: string): PreviewCall | null {
  const { outcome } = link;
  if (outcome.state === "not_deployed") {
    if (!open || outcome.why?.kind === "off") return null;
    return { text: "Deploy as a preview", url: confirm, markdown: `[Deploy as a preview](${confirm})` };
  }
  if (!open) return { text: "Preview closed", url: null, markdown: "Preview closed" };
  if (outcome.state === "closed") return { text: "Deploy as a preview", url: confirm, markdown: `[Deploy as a preview](${confirm})` };
  if (outcome.state === "deploying") return { text: "Deploying a preview…", url: confirm, markdown: `[Deploying a preview…](${confirm})` };
  const address = addressUrl(outcome.address);
  const opened = address === null
    ? { text: "Open preview", url: confirm, markdown: `[Open preview](${confirm})` }
    : { text: "Open preview", url: address, markdown: `[Open preview](${address}) ${address}` };
  if (!outcome.newer) return opened;
  return { text: "New commits: deploy again", url: confirm, markdown: `${opened.markdown} · New commits: [deploy again](${confirm})` };
}

/**
 * The pull request's one comment, from each linked Organization's view of it: null when no Store lists a preview
 * (nothing to say or take back), else its calls and the one line they make, null when none says anything (previews off
 * everywhere, or a closed pull request that never had one), which takes the comment down. Each call leads to that
 * Project's confirm page, or to the live preview.
 */
export function previewComment(
  views: readonly { slug: string; view: PreviewsView }[],
  pull: { repositoryId: number; number: number },
  appUrl: string,
): { calls: PreviewCall[]; line: string | null } | null {
  const linked = views.flatMap(({ slug, view }) => previewLinks(view).map((link) => ({ slug, link, open: view.pull_request?.open ?? true })));
  if (linked.length === 0) return null;
  const spoken = linked.flatMap(({ slug, link, open }) => {
    const confirm = new URL(`cloud/${slug}/${link.project}/previews/${pull.repositoryId}/${pull.number}`, appUrl).href;
    const said = call(link, open, confirm);
    return said === null ? [] : [{ project: link.project, said }];
  });
  // Several Projects preview the repository: each call names its Project.
  const calls = spoken.length < 2 ? spoken.map(({ said }) => said) : spoken.map(({ project, said }) =>
    ({ ...said, text: `${project}: ${said.text}`, markdown: `${project}: ${said.markdown}` }));
  return { calls, line: calls.length === 0 ? null : calls.map((each) => each.markdown).join(" · ") };
}
