/**
 * How long a Server may go without a signed push before Cloud counts it offline: three missed refreshes. An idle
 * Server pushes about once an hour, when its analytics salts turn an hour old (ployzd `observe/salts.rs` `FRESH_FOR`),
 * checked every 30 seconds (`observe/ship.rs` `TICK`). A Server serving traffic pushes its requests more often.
 */
export const SERVER_SILENT_AFTER_MS = 3 * 60 * 60 * 1000;

/** What Cloud stores of one Server's contact: when it was enrolled, whether it holds a push URL, and its last push. */
export type ServerContact = {
  readonly enrolledAt: Date;
  readonly pushTarget: string | null;
  readonly lastPushAt: Date | null;
};

/**
 * Whether every Server of the Organization has gone silent. A Server Cloud hasn't given its push URL can't push, so its
 * silence proves nothing; a Server that never pushed counts from its enrollment.
 */
export function serversOffline(servers: ReadonlyArray<ServerContact>, now: number): boolean {
  return servers.length > 0 && servers.every((server) =>
    server.pushTarget !== null && now - (server.lastPushAt ?? server.enrolledAt).getTime() > SERVER_SILENT_AFTER_MS);
}
