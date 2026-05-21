pub(crate) fn assert_eq_named<T>(name: &str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        return Ok(());
    }
    Err(format!("{name}: expected {expected:?}, got {actual:?}"))
}

pub(crate) fn expect_error<T, E>(name: &str, result: Result<T, E>) -> Result<E, String> {
    match result {
        Ok(_) => Err(format!("{name}: expected error, got success")),
        Err(error) => Ok(error),
    }
}

pub(crate) fn assert_error<E>(
    name: &str,
    error: &E,
    predicate: impl FnOnce(&E) -> bool,
) -> Result<(), String>
where
    E: std::fmt::Debug,
{
    if predicate(error) {
        return Ok(());
    }
    Err(format!("{name}: unexpected error {error:?}"))
}
