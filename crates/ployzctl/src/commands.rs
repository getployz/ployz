//! Small CLI command contracts.

pub mod deploy;
pub mod init;
pub mod machine;
pub mod ops;

pub const USAGE: &str = "\
ployzctl init --node <id> [--gateway]
ployzctl deploy --detach --service <id> --revision <id> --image <ref> --replicas <n>
ployzctl ops watch <operation_id>
ployzctl machine add --name <node> --join-token <token>";
