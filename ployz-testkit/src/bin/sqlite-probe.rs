use std::{env, error::Error, fs};

use rusqlite::{Connection, params};

fn main() -> Result<(), Box<dyn Error>> {
    let path = env::args().nth(1).ok_or("database path is required")?;
    let connection = Connection::open(&path)?;
    connection.execute("CREATE TABLE probe (value TEXT NOT NULL)", [])?;
    connection.execute("INSERT INTO probe VALUES (?1)", params!["ployz"])?;
    drop(connection);

    let connection = Connection::open(&path)?;
    let value: String = connection.query_row("SELECT value FROM probe", [], |row| row.get(0))?;
    fs::remove_file(path)?;
    if value != "ployz" {
        return Err("bundled SQLite did not persist the probe value".into());
    }
    Ok(())
}
