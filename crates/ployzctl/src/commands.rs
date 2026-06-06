//! Small CLI command contracts.

pub mod deploy;
pub mod machine;
pub mod ops;
pub mod upgrade;

pub const USAGE: &str = "\
ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n>
ployzctl ops watch <operation_id>
ployzctl machine add --name <node> --join-token <token>
ployzctl upgrade ployzd --version <version>";
