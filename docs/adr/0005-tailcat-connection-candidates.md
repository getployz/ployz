# Tailcat connections replace the hosted relay protocol

SSH and Tailcat share the existing Machine RPC Connector and identity confirmation. Cloud repurposes `organizationMachine` as encrypted backend-only connection metadata, scoped by Organization, Machine and current pairing; it is neither membership nor presence. This supersedes the protocol and slot decisions in 0001/0002 and the relay-hold completion condition in 0003, avoiding a second catalog or presence service.

The authenticated founder publishes its immutable candidate before completion. The current claim records the intended Machine as well as its public key; shared negotiation must confirm that Machine before transactional first-connect admission. Matching retries resume even after a lost completion response. Candidate absence and transport failure never authorize founder transfer or reset; reset remains unavailable until endpoint revocation can be confirmed.

The greenfield database history is a single baseline containing the current schema. It initializes fresh databases; it does not migrate existing pairings or authorize their reset. Existing databases require a separately planned cutover. Every new pairing must name a non-null 32-hex founding Machine ID.

The greenfield cutover removes the custom relay process, held registrations, tenant slots, and relay release/deployment artifacts. Supported Machine RPC connections use local sockets, SSH, or Tailcat; no compatibility relay backend remains. Releases bundle the selected native helper with the existing Machine lifecycle. Historical relay rationale remains in the explicitly superseded 0001/0002 records.
