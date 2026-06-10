# Keeper Update Is Separate From Substrate Update

Ployz v1 separates keeper update from substrate update because keeper is the machine-local executor for substrate steps. A full machine update may be exposed as CLI convenience, but core records keeper update and substrate update as separate explicit operations; substrate update requires keeper to already be at the requested Ployz version. This keeps update semantics small while still allowing keeper self-update through a dedicated handoff protocol.

