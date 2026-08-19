# Relay slots are keyed by Pairing Credential and Machine ID

Cloud at many organizations shares one Relay process and one Dial Credential. Register, Dial, List, and Revoke look up `(pairing, machineId)` from the Pairing Credential Machines present on Register and Cloud presents as metadata. Two customers may mint the same Machine ID; isolation is the map key, not a namespaced id. The Dial Credential authenticates Cloud; it does not select a partition.

Rejected: prefixing Machine ID with org/cluster/bearer, a global `UNIQUE(machine_id)`, one shared pairing for every customer, and Dial with only a Machine ID plus the process Dial Credential.
