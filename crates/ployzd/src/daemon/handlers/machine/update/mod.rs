mod installer;
mod local;
mod remote;
mod request;
#[cfg(test)]
mod tests;
mod version;

use ployz_api::MachineUpdateRow;
use ployz_model::MachineId;

fn update_row(id: &MachineId, version: &str, message: impl Into<String>) -> MachineUpdateRow {
    MachineUpdateRow {
        id: id.as_str().to_string(),
        version: version.to_string(),
        message: message.into(),
    }
}
