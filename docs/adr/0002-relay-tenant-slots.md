# Relay slots are keyed by Relay Tenant and Machine ID

Cloud at many organizations shares one Relay process. Register and Dial look up `(tenant, machineId)` from that organization's Pairing Credential or Dial Credential. Two customers may mint the same Machine ID; isolation is the map key, not a namespaced id.

Rejected: prefixing Machine ID with org/cluster/bearer, a global `UNIQUE(machine_id)`, one shared pairing/dial for every customer, and Dial with only a Machine ID plus a process-wide secret.
