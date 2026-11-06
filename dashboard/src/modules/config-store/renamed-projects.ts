/**
 * Projects this tab renamed, by Organization: each old name to the Project's current name.
 * The Store keeps no old names, so only this tab's own history entries follow a rename, and only until it reloads.
 * Only the rename's click handler writes it, so on the server it stays empty.
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

/**
 * Where a page under a Project's old name opens: the same page under its current name.
 * Undefined when this tab renamed no Project from that name, or a Project holds the name again.
 */
export function renamedProjectHref(
  organizationSlug: string,
  projectSlug: string,
  location: { pathname: string; searchStr: string; hash: string },
  projects: readonly { name: string }[],
) {
  const current = renamedProject(organizationSlug, projectSlug);
  if (current === undefined || projects.some((project) => project.name === projectSlug)) return undefined;
  const from = `/cloud/${organizationSlug}/${projectSlug}`;
  if (location.pathname !== from && !location.pathname.startsWith(`${from}/`)) return undefined;
  const hash = location.hash ? `#${location.hash}` : "";
  return `/cloud/${organizationSlug}/${current}${location.pathname.slice(from.length)}${location.searchStr}${hash}`;
}
