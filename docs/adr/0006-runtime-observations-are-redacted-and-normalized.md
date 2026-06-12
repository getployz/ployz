# Runtime Observations Are Redacted And Normalized

Ployz should store product-shaped runtime observations, not raw Docker inspect output or raw role-process dumps. Before writing observations to JetStream, Object Store evidence, operation events, or SDK-visible state, producers must remove secrets and normalize volatile ordering and noise.

Container observations must not include environment variables, auth tokens, certificate private material, secret values, or other fields that turn shared control-plane state into a secret sink. Docker-derived list fields with unstable ordering should be sorted or represented as sets so no-op observation cycles do not churn state.

This adopts Uncloud's container observation hygiene as a hard Ployz boundary. The trade-off is losing some low-level debugging detail in shared state; detailed artifacts can still exist as explicit retained evidence with redaction rules and scoped access.
