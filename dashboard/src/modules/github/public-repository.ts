/** Parse only repository identities, never arbitrary fetch destinations or branch URLs. */
export function normalizePublicGithubRepository(input: string): string | null {
  const match = /^(?:(?:https?:\/\/)?github\.com\/)?([a-z\d](?:[a-z\d-]{0,38}))\/([a-z\d_.-]{1,104})\/?$/i.exec(input.trim());
  if (!match) return null;
  const owner = match[1];
  const name = match[2]?.replace(/\.git$/i, "");
  if (!owner || !name || name.length > 100 || name === "." || name === "..") return null;
  return `${owner}/${name}`;
}
