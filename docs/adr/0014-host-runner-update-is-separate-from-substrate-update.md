# Host Runner Update Is Separate From Substrate Update

Ployz v1 separates Host Runner update from substrate update because Host Runner is the machine-local executor for substrate steps. A full machine update may be exposed as CLI convenience, but core records Host Runner update and substrate update as separate explicit operations; substrate update requires Host Runner to already be at the requested Ployz version. This keeps update semantics small while still allowing Host Runner self-update through a dedicated handoff protocol.

