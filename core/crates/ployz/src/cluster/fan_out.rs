//! One read to many Machines at once, kept as a Partial Result in the order the targets
//! were given. A target that does not invite an RPC is an omission, and an answer of
//! `None` is one too.
//!
//! Only this shape lives here. A fan-out that stops early or reports in the order Machines
//! answer has other semantics and keeps its own loop.

use std::future::Future;

use futures_util::future::join_all;
use ployz_core::{Machine, MachineId, MachineObservation, PartialResult};

/// What a fan-out needs to know of a target before it calls it.
pub(crate) trait Target {
    fn machine_id(&self) -> MachineId;

    /// False when the Machine is left out of the fan-out as an omission.
    fn invites_rpc(&self) -> bool {
        true
    }
}

impl Target for MachineObservation {
    fn machine_id(&self) -> MachineId {
        self.machine.id
    }

    fn invites_rpc(&self) -> bool {
        MachineObservation::invites_rpc(self)
    }
}

impl Target for Machine {
    fn machine_id(&self) -> MachineId {
        self.id
    }
}

impl Target for MachineId {
    fn machine_id(&self) -> MachineId {
        *self
    }
}

impl<T: Target + ?Sized> Target for &T {
    fn machine_id(&self) -> MachineId {
        (**self).machine_id()
    }

    fn invites_rpc(&self) -> bool {
        (**self).invites_rpc()
    }
}

/// Call every target that invites an RPC at once. `call` gives each target its own
/// future; clone the `Client` inside it, since a call on `&mut Client` redials its own fork.
pub(crate) async fn fan_out<Each, T, E, Fut>(
    targets: impl IntoIterator<Item = Each>,
    call: impl Fn(Each) -> Fut,
) -> PartialResult<T, E>
where
    Each: Target,
    Fut: Future<Output = Result<Option<T>, E>>,
{
    fan_out_entry_last(targets, None, call).await
}

/// [`fan_out`], calling the `entry` Machine only after every other has answered: a Device
/// change that would close this CLI's own path goes last. Answers keep the targets' order;
/// the entry's failure or omission is recorded last.
pub(crate) async fn fan_out_entry_last<Each, T, E, Fut>(
    targets: impl IntoIterator<Item = Each>,
    entry: Option<MachineId>,
    call: impl Fn(Each) -> Fut,
) -> PartialResult<T, E>
where
    Each: Target,
    Fut: Future<Output = Result<Option<T>, E>>,
{
    let mut result = PartialResult::empty();
    let mut asked = Vec::new();
    let mut others = Vec::new();
    let mut last = None;
    for target in targets {
        let machine_id = target.machine_id();
        if !target.invites_rpc() {
            result.omissions.push(machine_id);
            continue;
        }
        asked.push(machine_id);
        let call = call(target);
        if Some(machine_id) == entry {
            last = Some((machine_id, call));
        } else {
            others.push(async move { (machine_id, call.await) });
        }
    }
    for (machine_id, outcome) in join_all(others).await {
        result.record(machine_id, outcome);
    }
    if let Some((machine_id, call)) = last {
        result.record(machine_id, call.await);
    }
    result
        .successes
        .sort_by_key(|success| asked.iter().position(|id| *id == success.machine_id));
    result
}

#[cfg(test)]
mod tests {
    use std::{sync::Mutex, time::Duration};

    use ployz_core::MembershipObservation;

    use super::*;

    fn id(hex: char) -> MachineId {
        MachineId::parse(hex.to_string().repeat(32)).unwrap()
    }

    fn observed(hex: char, membership: MembershipObservation) -> MachineObservation {
        MachineObservation::new(
            crate::global_fixtures::machine(hex, &format!("machine-{hex}")),
            membership,
        )
    }

    async fn after(
        millis: u64,
        answer: Result<Option<char>, String>,
    ) -> Result<Option<char>, String> {
        tokio::time::sleep(Duration::from_millis(millis)).await;
        answer
    }

    #[tokio::test(start_paused = true)]
    async fn answers_keep_the_order_the_targets_were_given_whichever_answers_first() {
        let machines = [
            observed('a', MembershipObservation::Up),
            observed('b', MembershipObservation::Down),
            observed('c', MembershipObservation::Up),
            observed('d', MembershipObservation::Up),
            observed('e', MembershipObservation::Up),
        ];
        let result = fan_out(&machines, |machine| {
            let hex = machine.machine.name.as_str().chars().last().unwrap();
            match hex {
                'a' => after(30, Ok(Some('a'))),
                'c' => after(10, Err("c".into())),
                'd' => after(20, Ok(None)),
                _ => after(0, Ok(Some('e'))),
            }
        })
        .await;
        let answered: Vec<_> = result
            .successes
            .iter()
            .map(|each| (each.machine_id, each.value))
            .collect();
        assert_eq!(answered, [(id('a'), 'a'), (id('e'), 'e')]);
        assert_eq!(
            result
                .failures
                .iter()
                .map(|each| (each.machine_id, each.error.as_str()))
                .collect::<Vec<_>>(),
            [(id('c'), "c")]
        );
        assert_eq!(result.omissions, [id('b'), id('d')]);
    }

    #[tokio::test(start_paused = true)]
    async fn the_entry_is_called_after_every_other_target_and_answers_in_its_place() {
        let calls = Mutex::new(Vec::new());
        let targets = [id('1'), id('2'), id('3')];
        let result = fan_out_entry_last(targets, Some(id('1')), |target| {
            let calls = &calls;
            async move {
                // The entry would answer first if it were not held back.
                let delay = if target == id('1') { 0 } else { 50 };
                tokio::time::sleep(Duration::from_millis(delay)).await;
                calls.lock().unwrap().push(target);
                if target == id('3') {
                    Err("3".to_owned())
                } else {
                    Ok(Some(()))
                }
            }
        })
        .await;
        assert_eq!(*calls.lock().unwrap(), [id('2'), id('3'), id('1')]);
        let answered: Vec<_> = result
            .successes
            .iter()
            .map(|each| each.machine_id)
            .collect();
        assert_eq!(answered, [id('1'), id('2')]);
        assert_eq!(result.failures.len(), 1);
    }
}
