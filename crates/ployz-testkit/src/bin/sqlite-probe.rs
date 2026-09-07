use std::{env, fs};

use rusqlite::{Connection, params};
use thiserror::Error;

#[derive(Debug, Error)]
enum Error {
    #[error("database path is required")]
    MissingPath,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("bundled SQLite did not persist the probe value")]
    Persist,
}

fn main() -> Result<(), Error> {
    let path = env::args().nth(1).ok_or(Error::MissingPath)?;
    let connection = Connection::open(&path)?;
    connection.execute("CREATE TABLE probe (value TEXT NOT NULL)", [])?;
    connection.execute("INSERT INTO probe VALUES (?1)", params!["ployz"])?;
    drop(connection);

    let connection = Connection::open(&path)?;
    let value: String = connection.query_row("SELECT value FROM probe", [], |row| row.get(0))?;
    fs::remove_file(path)?;
    if value != "ployz" {
        return Err(Error::Persist);
    }
    Ok(())
}
