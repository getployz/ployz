/**
 * Projects this tab renamed, by Organization: each old name to the Project's current name.
 * The Store keeps no old names, so only this tab's own history entries follow a rename, and only until it reloads.
 */
const renamed = new Map<string, Map<string, string>>();

export function rememberProjectRename(organizationSlug: string, from: string, to: string) {
  const names = renamed.get(organizationSlug) ?? new Map<string, string>();
  for (const [old, current] of names) if (current === from) names.set(old, to);
  // `to` names a Project again, so it no longer leads anywhere else.
  names.delete(to);
  names.set(from, to);
  renamed.set(organizationSlug, names);
}

/** The current name of a Project this tab renamed away from `projectSlug`; undefined if it renamed none. */
export function renamedProject(organizationSlug: string, projectSlug: string) {
  return renamed.get(organizationSlug)?.get(projectSlug);
}
