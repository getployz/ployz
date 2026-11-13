//! What a forget tells the operator, because nothing reaches a forgotten Server to fence it
//! (#2927), and the answer for a Server that was already forgotten.

use ployz_core::Machine;
use ployz_core::MachineName;
use serde_json::json;

use crate::handlers::Error;

/// What a forgotten Server still runs, and how it is told.
pub(super) enum Forgotten<'a> {
    /// Every Server of the Organization, which no daemon reached.
    All,
    /// A Server that did not answer, so it only learns of it by itself, if at all.
    Dead(&'a MachineName),
    /// A Server that answers and still runs.
    Answering(&'a MachineName),
}

const UNINSTALL_STEP: &str = "its services start again on its own copy of each Volume, and Ployz \
     never brings back what they write there. When it boots, run sudo ployz-uninstall on it to \
     stop them and keep every Volume copy on its disk.";
const RESET_CAVEAT: &str = "Adding it back with ployz server add --reset keeps its writer copies \
     read-only but can delete its mirrors, so don't while one may be a Volume's last copy.";
const WIPE_LAST: &str = "Wipe its disk only if you need nothing on it.";

impl Forgotten<'_> {
    /// The warning for the operator. A Server reachable now may restore its own row, so only a
    /// dead one is told about the self-fence; it is best effort, so the operator step stays.
    fn warning(&self) -> String {
        match self {
            Self::All => format!(
                "A forgotten Server doesn't know it was forgotten. If one runs again as it is, \
                 {UNINSTALL_STEP} {RESET_CAVEAT} {WIPE_LAST}"
            ),
            Self::Dead(server) => format!(
                "Server {server} isn't told it was forgotten. If it runs again as it is, \
                 {UNINSTALL_STEP} As a best effort, if {server} and at least one other Server \
                 ran 0.3.1 or later while it was still connected, and the Server that forgot it \
                 runs 0.3.1 or later, it does that itself up to about 15 minutes after its \
                 network returns. {RESET_CAVEAT} {WIPE_LAST}"
            ),
            Self::Answering(server) => format!(
                "Server {server} still runs and doesn't know it was forgotten, so its services \
                 keep running there. Run sudo ployz-uninstall on it to stop them and keep every \
                 Volume copy on its disk. Adding it back with ployz server add --reset can \
                 delete its mirrors, so don't while one may be a Volume's last copy."
            ),
        }
    }

    pub(super) fn warn(&self) {
        crate::ui::warn(self.warning());
    }
}

/// The result of a forget of a Server another forget already took out.
pub(super) fn already_forgotten(selected: &Machine) -> Result<(), Error> {
    crate::ui::stream(format_args!(
        "Server {} was already forgotten. Nothing to do.",
        selected.name
    ));
    crate::ui::emit(&json!({
        "server": super::server_json(selected),
        "already_forgotten": true,
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_forget_warns_in_its_own_words() {
        let server = MachineName::parse("two").unwrap();
        assert_eq!(
            Forgotten::All.warning(),
            "A forgotten Server doesn't know it was forgotten. If one runs again as it is, its \
             services start again on its own copy of each Volume, and Ployz never brings back \
             what they write there. When it boots, run sudo ployz-uninstall on it to stop them \
             and keep every Volume copy on its disk. Adding it back with ployz server add \
             --reset keeps its writer copies read-only but can delete its mirrors, so don't \
             while one may be a Volume's last copy. Wipe its disk only if you need nothing on it."
        );
        assert_eq!(
            Forgotten::Dead(&server).warning(),
            "Server two isn't told it was forgotten. If it runs again as it is, its services \
             start again on its own copy of each Volume, and Ployz never brings back what they \
             write there. When it boots, run sudo ployz-uninstall on it to stop them and keep \
             every Volume copy on its disk. As a best effort, if two and at least one other \
             Server ran 0.3.1 or later while it was still connected, and the Server that forgot \
             it runs 0.3.1 or later, it does that itself up to about 15 minutes after its \
             network returns. Adding it back with ployz server add --reset keeps its writer \
             copies read-only but can delete its mirrors, so don't while one may be a Volume's \
             last copy. Wipe its disk only if you need nothing on it."
        );
        assert_eq!(
            Forgotten::Answering(&server).warning(),
            "Server two still runs and doesn't know it was forgotten, so its services keep \
             running there. Run sudo ployz-uninstall on it to stop them and keep every Volume \
             copy on its disk. Adding it back with ployz server add --reset can delete its \
             mirrors, so don't while one may be a Volume's last copy."
        );
    }
}
