/**
 * Every Postgres advisory lock number Cloud takes by a fixed number rather than by `hashtext` of a name, in one place so
 * no two meet. The two-key locks use the number as their class and `hashtext` or an id as the second key, which keeps
 * them apart from every one-key lock. Changing a number lets old and new Cloud processes take the same lock at once
 * during a rollout, so a number never changes.
 */
export const ADVISORY_LOCK = {
  /** Serializes the writers that create or delete an Organization's enrollment state; keyed by Organization. */
  organizationEnrollment: 1_349_601,
  /** Serializes the writers of one GitHub installation's cached repositories; keyed by installation id. */
  githubRepositoryCache: 1_349_602,
  /** Provisioning and revoking one credential's Management Client holders take turns; keyed by credential. */
  credential: 1_349_603,
  /** Serializes the processes that attach the Store's change log at once; one key, the only whole-Store lock. */
  storeChangeLog: 1256,
} as const;
