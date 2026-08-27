use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};

use crate::{CachedSurfaceCoverage, RestorationCoverage, RestoreError};

pub(crate) fn validate_restoration_coverage_schema(
    coverage: &RestorationCoverage,
) -> Result<(), RestoreError> {
    validate_schema_profile(
        coverage.all_tables.iter().map(|table| {
            (
                table.source_logical_path.as_str(),
                table.source_table_name.as_str(),
                table.schema_fingerprint.as_deref(),
            )
        }),
        coverage.schema_profile_fingerprint.as_deref(),
        coverage.format_version >= 3,
    )
}

pub(crate) fn validate_cached_coverage_schema(
    coverage: &CachedSurfaceCoverage,
) -> Result<(), RestoreError> {
    validate_schema_profile(
        coverage.tables.iter().map(|table| {
            (
                table.source_logical_path.as_str(),
                table.source_table_name.as_str(),
                table.schema_fingerprint.as_deref(),
            )
        }),
        coverage.schema_profile_fingerprint.as_deref(),
        coverage.format_version >= 2,
    )
}

pub(crate) fn table_schema_fingerprint(
    connection: &Connection,
    table: &str,
) -> Result<String, RestoreError> {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"greenbubbles-table-schema-v1");
    hash_field(&mut hasher, table.as_bytes());

    let pragma = format!("PRAGMA table_xinfo({})", quote_identifier(table));
    let mut columns = connection.prepare(&pragma)?;
    let rows = columns.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    })?;
    for row in rows {
        let (ordinal, name, declared_type, not_null, default_value, primary_key, hidden) = row?;
        hash_i64(&mut hasher, ordinal);
        hash_field(&mut hasher, name.as_bytes());
        hash_field(&mut hasher, declared_type.as_bytes());
        hash_i64(&mut hasher, not_null);
        hash_optional(&mut hasher, default_value.as_deref());
        hash_i64(&mut hasher, primary_key);
        hash_i64(&mut hasher, hidden);
    }

    let mut objects = connection.prepare(
        "SELECT type, name, tbl_name, sql
         FROM sqlite_schema
         WHERE (name = ?1 OR tbl_name = ?1) AND name NOT LIKE 'sqlite_%'
         ORDER BY type, name, tbl_name, rowid",
    )?;
    let rows = objects.query_map(params![table], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for row in rows {
        let (kind, name, table_name, sql) = row?;
        hash_field(&mut hasher, kind.as_bytes());
        hash_field(&mut hasher, name.as_bytes());
        hash_field(&mut hasher, table_name.as_bytes());
        hash_optional(&mut hasher, sql.as_deref());
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn schema_profile_fingerprint<'a>(
    tables: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
) -> Option<String> {
    let mut tables = tables.into_iter().collect::<Vec<_>>();
    if tables
        .iter()
        .any(|(_, _, fingerprint)| fingerprint.is_none())
    {
        return None;
    }
    tables.sort_unstable();
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, b"greenbubbles-schema-profile-v1");
    for (logical_path, table_name, fingerprint) in tables {
        hash_field(&mut hasher, logical_path.as_bytes());
        hash_field(&mut hasher, table_name.as_bytes());
        hash_field(&mut hasher, fingerprint.expect("checked above").as_bytes());
    }
    Some(hex::encode(hasher.finalize()))
}

pub(crate) fn validate_schema_profile<'a>(
    tables: impl IntoIterator<Item = (&'a str, &'a str, Option<&'a str>)>,
    observed: Option<&str>,
    required: bool,
) -> Result<(), RestoreError> {
    let tables = tables.into_iter().collect::<Vec<_>>();
    if tables
        .iter()
        .any(|(_, _, fingerprint)| fingerprint.is_some_and(|fingerprint| !is_sha256(fingerprint)))
        || observed.is_some_and(|fingerprint| !is_sha256(fingerprint))
    {
        return Err(RestoreError::Integrity(
            "schema coverage contains a malformed fingerprint".to_string(),
        ));
    }
    let expected = schema_profile_fingerprint(tables.iter().copied());
    if required && (expected.is_none() || observed.is_none()) {
        return Err(RestoreError::Integrity(
            "current schema coverage is missing fingerprint evidence".to_string(),
        ));
    }
    if let Some(observed) = observed {
        if expected.as_deref() != Some(observed) {
            return Err(RestoreError::Integrity(
                "schema profile fingerprint does not match its table ledger".to_string(),
            ));
        }
    }
    Ok(())
}

fn hash_i64(hasher: &mut Sha256, value: i64) {
    hash_field(hasher, &value.to_le_bytes());
}

fn hash_optional(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hash_field(hasher, b"some");
            hash_field(hasher, value.as_bytes());
        }
        None => hash_field(hasher, b"none"),
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_fingerprints_ignore_rows_but_detect_structural_drift() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE Msg_test(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
                 CREATE INDEX Msg_test_body ON Msg_test(body);",
            )
            .unwrap();
        let initial = table_schema_fingerprint(&connection, "Msg_test").unwrap();
        connection
            .execute("INSERT INTO Msg_test(body) VALUES (x'0102')", [])
            .unwrap();
        assert_eq!(
            table_schema_fingerprint(&connection, "Msg_test").unwrap(),
            initial
        );

        connection
            .execute("ALTER TABLE Msg_test ADD COLUMN created_at INTEGER", [])
            .unwrap();
        let changed = table_schema_fingerprint(&connection, "Msg_test").unwrap();
        assert_ne!(changed, initial);
        assert_eq!(initial.len(), 64);
        assert_eq!(changed.len(), 64);
    }

    #[test]
    fn profile_is_order_independent_and_propagates_missing_evidence() {
        let first = schema_profile_fingerprint([
            ("message/b.db", "Msg_b", Some("bbb")),
            ("message/a.db", "Msg_a", Some("aaa")),
        ]);
        let second = schema_profile_fingerprint([
            ("message/a.db", "Msg_a", Some("aaa")),
            ("message/b.db", "Msg_b", Some("bbb")),
        ]);
        assert_eq!(first, second);
        assert_eq!(first.unwrap().len(), 64);
        assert!(schema_profile_fingerprint([("message/a.db", "Msg_a", None)]).is_none());
    }

    #[test]
    fn validation_rejects_missing_malformed_and_mismatched_current_evidence() {
        let table_fingerprint = "a".repeat(64);
        let tables = || [("message/a.db", "Msg_a", Some(table_fingerprint.as_str()))];
        let expected = schema_profile_fingerprint(tables()).unwrap();
        validate_schema_profile(tables(), Some(&expected), true).unwrap();
        assert!(validate_schema_profile(tables(), None, true).is_err());
        assert!(validate_schema_profile(
            [("message/a.db", "Msg_a", Some("not-a-hash"))],
            Some(&expected),
            true,
        )
        .is_err());
        assert!(validate_schema_profile(tables(), Some(&"b".repeat(64)), true).is_err());
    }
}
