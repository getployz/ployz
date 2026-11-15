//! On-demand Container log transport for SDK readers: `stream` reads Docker and the Log
//! Store, `tail` the failed Container's last lines for a deploy, `agent` the lines an agent reads.

mod agent;
mod stream;
mod tail;

pub(crate) use agent::{AgentTail, process_tail, service_tail};
pub(crate) use stream::container_env;
pub use stream::{
    ContainerLogInput, ContainerLogStream, LogHistoryInput, LogHistoryRecord, LogHistoryStream,
};
pub(super) use stream::{LogChannel, ROW_DEADLINE, history_record};
pub(crate) use tail::tail_frames;
