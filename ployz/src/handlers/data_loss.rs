//! Named Data Loss confirmation shared by Machine and Project destroy.

use std::io::{self, IsTerminal, Write};

use ployz_core::{DataLoss, ObservedDataLoss};

use crate::failure::pass_data_loss_names_message;

use super::Error;

pub(super) fn collect_data_loss_confirmation(
    observed: &ObservedDataLoss,
    named: &[String],
) -> Result<Vec<DataLoss>, Error> {
    if !observed.data_loss.is_empty() {
        eprintln!("Data Loss:");
        for loss in &observed.data_loss {
            eprintln!("  {}", loss.name());
        }
    }
    let prompted;
    let names = if named.is_empty() && !observed.data_loss.is_empty() {
        prompted = read_data_loss_names(observed)?;
        prompted.as_slice()
    } else {
        named
    };
    let confirmation = observed.named(names.iter().map(String::as_str))?;
    let missing = observed.uncovered_by(&confirmation);
    if missing.is_empty() {
        Ok(confirmation)
    } else {
        Err(Error::usage(pass_data_loss_names_message(&missing)))
    }
}

fn read_data_loss_names(observed: &ObservedDataLoss) -> Result<Vec<String>, Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::usage(pass_data_loss_names_message(
            &observed.data_loss,
        )));
    }
    print!("Name the Data Loss to continue: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(answer.split_whitespace().map(str::to_owned).collect())
}

#[cfg(test)]
mod tests {
    use super::collect_data_loss_confirmation;
    use crate::failure::{pass_data_loss_names_message, refusal_from_rpc};
    use ployz_core::{
        DataLoss, DockerVolumeId, DockerVolumeName, MachineId, ObservedDataLoss,
        UnconfirmedDataLoss,
    };

    #[test]
    fn refusing_without_a_confirmation_states_which_names_to_pass() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('a', "logs")],
        };
        let error = collect_data_loss_confirmation(&observed, &["gone".into()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Data Loss is not covered by the confirmation; pass the names as arguments: data logs"
        );
    }

    #[test]
    fn typed_names_that_match_more_than_one_listed_entry_are_refused() {
        let observed = ObservedDataLoss {
            data_loss: vec![loss('a', "data"), loss('b', "data")],
        };
        let error = collect_data_loss_confirmation(&observed, &["data".into()]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Data Loss name \"data\" matches more than one listed entry"
        );
    }

    #[test]
    fn no_data_loss_needs_no_names() {
        let observed = ObservedDataLoss {
            data_loss: Vec::new(),
        };
        assert_eq!(
            collect_data_loss_confirmation(&observed, &[]).unwrap(),
            Vec::<DataLoss>::new()
        );
    }

    #[test]
    fn execute_time_unconfirmed_data_loss_states_which_names_to_pass() {
        let missing = vec![loss('a', "logs")];
        let error = UnconfirmedDataLoss {
            missing: missing.clone(),
        }
        .into_rpc_error();
        assert_eq!(
            refusal_from_rpc(error).to_string(),
            pass_data_loss_names_message(&missing)
        );
    }

    fn loss(machine: char, name: &str) -> DataLoss {
        DataLoss::DockerVolume(DockerVolumeId {
            machine_id: machine_id(machine),
            name: DockerVolumeName::parse(name).unwrap(),
        })
    }

    fn machine_id(value: char) -> MachineId {
        MachineId::parse(value.to_string().repeat(32)).unwrap()
    }
}
