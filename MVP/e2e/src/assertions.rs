use mvp_bus::BusError;

pub(crate) fn assert_eq_named<T>(name: &str, actual: T, expected: T) -> Result<(), String>
where
    T: std::fmt::Debug + PartialEq,
{
    if actual == expected {
        return Ok(());
    }
    Err(format!("{name}: expected {expected:?}, got {actual:?}"))
}

pub(crate) fn expect_error<T>(name: &str, result: Result<T, BusError>) -> Result<BusError, String> {
    match result {
        Ok(_) => Err(format!("{name}: expected error, got success")),
        Err(error) => Ok(error),
    }
}

pub(crate) fn assert_error(
    name: &str,
    error: &BusError,
    predicate: impl FnOnce(&BusError) -> bool,
) -> Result<(), String> {
    if predicate(error) {
        return Ok(());
    }
    Err(format!("{name}: unexpected error {error}"))
}
