/**
 * A Store write's body, as JSON, with its `shows` in a fixed order. The writer lists shown views newest first by the
 * millisecond each was last answered, so two views one write answered can swap on a busy run.
 */
export function anyOrderShows(json: string): string {
  const { shows, ...body }: { shows: unknown[] } = JSON.parse(json);
  return JSON.stringify({ ...body, shows: shows.map((view) => JSON.stringify(view)).sort() });
}
