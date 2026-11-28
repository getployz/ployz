/**
 * Previews as the Store answers them: the query builders, and a kept rule's body. The types are the SDK's, with the JSON
 * field names DESIGN.md (NIC-33) fixes between the Store, the rule draft and this dashboard.
 */
import type { AutoDeployRule, PreviewRuleQuery, PreviewsQuery, Signed } from "@ployz/sdk";

/** One repository's previews in a Project, and the pull requests that may have one. */
export function previewsQuery(query: PreviewsQuery): { query: "previews" } & PreviewsQuery {
  return { query: "previews", ...query };
}

/** What a preview rule for `repository` copying `start_from` would cover. */
export function previewRuleQuery(query: PreviewRuleQuery): { query: "preview_rule" } & PreviewRuleQuery {
  return { query: "preview_rule", ...query };
}

/** A kept rule's body as ployz-core wrote it. */
export function ruleBody(rule: Signed): AutoDeployRule | null {
  try {
    // SAFETY: the Store keeps only a rule body it parsed as an `AutoDeployRule`.
    const body: AutoDeployRule = JSON.parse(rule.body);
    return body;
  } catch {
    return null;
  }
}
