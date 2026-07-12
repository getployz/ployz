# Endpoint Subnet Assignment

Machine endpoint subnets are core-owned intent allocated when a machine-add
operation is admitted. The allocation and persisted submission share the mesh
sequencer lock, and both active machines and pending machine-add submissions
reserve their subnets.

The assigned `MachineEndpointSubnet` is stored in the machine-add identity.
Join redemption delivers that exact value to Host Runner, which writes
`PLOYZ_DATAPLANE_ENDPOINT_SUBNET` for the machine and DNS roles. Connectivity
proof and roster activation read the same stored value.

This single allocation point prevents two failures:

- distinct machine ids whose derived defaults collide receiving the same subnet;
- roster changes between redemption and activation changing the subnet after a
  machine has configured WireGuard, the image registry, and DNS.

The machine-id-derived subnet remains the preferred allocation when free. The
configured endpoint supernet supplies the next free `/24` when it is occupied.
