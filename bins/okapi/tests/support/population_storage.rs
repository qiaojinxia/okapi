//! Remove or corrupt both physical generations to simulate missing measurements.
//! Leaving classified states intact is usable history, not a pre-upgrade gap.
use okapi_store::{ChClient, StoreError};

pub(super) async fn execute(ch: &ChClient, sql: &str) -> Result<(), StoreError> {
    ch.execute(sql).await?;
    let mut words = sql.split_whitespace();
    let (Some(action), Some(kind), Some(mut table)) = (words.next(), words.next(), words.next())
    else {
        return Ok(());
    };
    if table == "IF" && words.next() == Some("EXISTS") {
        let Some(name) = words.next() else {
            return Ok(());
        };
        table = name;
    }
    if !table.starts_with("mv_") {
        return Ok(());
    }
    let mutation = matches!(action, "DROP" | "TRUNCATE" | "ALTER") && kind == "TABLE";
    let copy = action == "INSERT"
        && kind == "INTO"
        && sql.contains("SELECT *")
        && sql.contains(&format!("FROM {table}"));
    if mutation || copy {
        let mirrored = sql.replace(table, &format!("population_v1_{table}"));
        ch.execute(&mirrored).await?;
    }
    Ok(())
}
