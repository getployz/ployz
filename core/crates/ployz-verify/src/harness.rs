//! The test harness every model shares: the whole state space, the first path that breaks a
//! property or reaches a `sometimes`, and a schedule followed step by step.
//!
//! A model keeps its `story`, its start states and its assertions on a path. The checker's
//! own discovery is not the first path in BFS order, since `check_block` stores every
//! violation it visits, each overwriting the last, and `finish_when` is read once per
//! 1500-state block. So `first_break` keeps a visitor's first match, on one thread so the
//! path is the same from run to run.

use std::{
    collections::BTreeSet,
    fmt::Debug,
    hash::Hash,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use stateright::{Checker, Expectation, HasDiscoveries, Model};

pub(crate) type Path<M> = Vec<(<M as Model>::State, Option<<M as Model>::Action>)>;

/// A model that can print a path as a story under the test that found it.
pub(crate) trait Narrated:
    Model<
        State: Clone + Debug + Hash + PartialEq + Send + Sync + 'static,
        Action: Clone + Debug + PartialEq + Send + Sync + 'static,
    > + Clone
    + Debug
    + Send
    + Sync
    + 'static
{
    fn story(&self, path: &[(Self::State, Option<Self::Action>)]) -> String;
}

/// Every state of `model`. The default, `HasDiscoveries::All`, stops once every property has
/// a discovery, at a state count that varies with the threads; an empty `AnyOf` never
/// matches, so the count is the same from run to run.
pub(crate) fn check<M>(model: M) -> impl Checker<M>
where
    M: Model + Send + Sync + 'static,
    M::State: Hash + Send + Sync + Clone + PartialEq + 'static,
    M::Action: Clone + PartialEq,
{
    model
        .checker()
        .threads(4)
        .finish_when(HasDiscoveries::AnyOf(BTreeSet::new()))
        .spawn_bfs()
        .join()
}

/// Explores until every property in `properties` has a discovery, on four threads. The
/// checker's own discovery for a property is any path that breaks it, not the first.
pub(crate) fn discoveries<M>(model: M, properties: &[&'static str]) -> impl Checker<M> + use<M>
where
    M: Model + Send + Sync + 'static,
    M::State: Hash + Send + Sync + Clone + PartialEq + 'static,
    M::Action: Clone + PartialEq,
{
    model
        .checker()
        .threads(4)
        .finish_when(HasDiscoveries::AllOf(properties.iter().copied().collect()))
        .spawn_bfs()
        .join()
}

/// The first path in BFS order whose last state breaks `property` under `model`, so no
/// shorter path breaks it, printed as a story under the test's name. For a `sometimes`, such
/// as a `COVERAGE` row's name, it is the first path that reaches it. It panics when no path
/// does, as for an `always` that holds.
pub(crate) fn breaks<M: Narrated>(model: M, property: &'static str) -> Path<M> {
    first_break(model, property, |_| true, true).expect("a path breaks the property")
}

/// As `breaks`, the first such path that also satisfies `and`. It explores the whole model,
/// since the first discovery may not pass `and`. The checker keeps one path to each state, so
/// `and` sees only the first path to the state it ends in.
pub(crate) fn breaks_where<M: Narrated>(
    model: M,
    property: &'static str,
    and: impl Fn(&Path<M>) -> bool + Send + Sync + 'static,
) -> Path<M> {
    first_break(model, property, and, false).expect("a path breaks the property")
}

/// As `breaks_where`, or `None` when no path breaks `property` and satisfies `and`.
///
/// It explores every state when `stop_at_discovery` is false, and asserts the checker
/// visited more than one state before it answers `None`, so a model that cannot leave its
/// start state never passes an absence by exploring nothing.
pub(crate) fn first_break<M: Narrated>(
    model: M,
    property: &'static str,
    and: impl Fn(&Path<M>) -> bool + Send + Sync + 'static,
    stop_at_discovery: bool,
) -> Option<Path<M>> {
    let named = model
        .properties()
        .into_iter()
        .find(|candidate| candidate.name == property)
        .expect("a model property");
    let reached = matches!(named.expectation, Expectation::Sometimes);
    first_path(
        model,
        property,
        named.condition,
        reached,
        and,
        stop_at_discovery.then_some(property),
    )
}

/// The first path in BFS order to a state where `holds` is `reached` and that satisfies
/// `and`, printed as a story under the test's name, or `None`. With `stop_at` it stops once
/// that property has a discovery; without it, it explores every state.
pub(crate) fn first_path<M: Narrated>(
    model: M,
    what: &str,
    holds: fn(&M, &M::State) -> bool,
    reached: bool,
    and: impl Fn(&Path<M>) -> bool + Send + Sync + 'static,
    stop_at: Option<&'static str>,
) -> Option<Path<M>> {
    let first = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&first);
    let visiting = model.clone();
    let checker = model
        .clone()
        .checker()
        .threads(1)
        .visitor(move |path: stateright::Path<M::State, M::Action>| {
            let mut first = seen.lock().expect("the first break");
            if first.is_none() && holds(&visiting, path.last_state()) == reached {
                let path = path.into_vec();
                if and(&path) {
                    *first = Some(path);
                }
            }
        })
        // An empty `AnyOf` never matches, so the checker visits every state.
        .finish_when(match stop_at {
            Some(property) => HasDiscoveries::AllOf([property].into()),
            None => HasDiscoveries::AnyOf(BTreeSet::new()),
        })
        .spawn_bfs()
        .join();
    let found = first.lock().expect("the first break").take();
    let Some(path) = found else {
        assert!(
            checker.unique_state_count() > 1,
            "{what} found nothing under {model:?} because it explored no state"
        );
        return None;
    };
    let test = std::thread::current();
    println!(
        "[{}] {what} {} under {model:?}:\n{}",
        test.name().unwrap_or("?"),
        if reached { "is reached" } else { "fails" },
        model.story(&path)
    );
    Some(path)
}

/// Whether any state of `model` satisfies `reached`, from a walk of every state.
pub(crate) fn reaches<M: Narrated>(
    model: M,
    reached: impl Fn(&M::State) -> bool + Send + Sync + 'static,
) -> bool {
    let found = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&found);
    let checker = model
        .checker()
        .threads(4)
        .visitor(move |path: stateright::Path<M::State, M::Action>| {
            if reached(path.last_state()) {
                seen.store(true, Ordering::Relaxed);
            }
        })
        .finish_when(HasDiscoveries::AnyOf(BTreeSet::new()))
        .spawn_bfs()
        .join();
    assert!(
        checker.unique_state_count() > 1,
        "the walk visited no state"
    );
    found.load(Ordering::Relaxed)
}

/// The state a path ends in.
pub(crate) fn last<S: Clone, A>(path: &[(S, Option<A>)]) -> S {
    path.last().expect("a path has a state").0.clone()
}

/// `action` was taken somewhere on `path`.
pub(crate) fn took_action<S, A: PartialEq>(path: &[(S, Option<A>)], action: A) -> bool {
    path.iter()
        .any(|(_, taken)| taken.as_ref() == Some(&action))
}

/// Steps `actions` from `start` under `model`, each one offered where it is taken, and
/// prints the path as a story under the test's name.
pub(crate) fn follow_from<M: Narrated>(
    model: M,
    start: M::State,
    actions: &[M::Action],
) -> Path<M> {
    assert!(model.init_states().contains(&start), "{start:?} is a start");
    let mut state = start;
    let mut path = Vec::new();
    for action in actions {
        let mut offered = Vec::new();
        model.actions(&state, &mut offered);
        assert!(offered.contains(action), "{action:?} is offered");
        let next = model
            .next_state(&state, action.clone())
            .expect("a next state");
        path.push((state, Some(action.clone())));
        state = next;
    }
    path.push((state, None));
    let test = std::thread::current();
    println!(
        "[{}] a schedule under {model:?}:\n{}",
        test.name().unwrap_or("?"),
        model.story(&path)
    );
    path
}

/// As `follow_from`, from the model's first start state.
pub(crate) fn follow<M: Narrated>(model: M, actions: &[M::Action]) -> Path<M> {
    let start = model
        .init_states()
        .into_iter()
        .next()
        .expect("a start state");
    follow_from(model, start, actions)
}
