use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use base64::Engine;
use rusqlite::{types::ValueRef, Connection, OpenFlags, Row};
use sha2::{Digest, Sha256};

use crate::restore::scoped_opaque_id;
use crate::{
    CachedMomentInteractionKind, CachedSurfaceCompleteness, CachedSurfaceCoverage,
    CachedSurfaceTableCoverage, CachedSurfaceTableRole, CanonicalCachedMoment,
    CanonicalCachedMomentInteraction, PreparedCatalog, RawSQLiteValue, RestoreError,
    SemanticDecodeState,
};

pub struct CachedSurfaceRestoration {
    pub moments_path: PathBuf,
    pub interactions_path: PathBuf,
    pub coverage_path: PathBuf,
    pub coverage: CachedSurfaceCoverage,
}

pub fn restore_cached_surfaces(
    catalog: &PreparedCatalog,
    account_id: &str,
    output_directory: &Path,
) -> Result<CachedSurfaceRestoration, RestoreError> {
    let moments_path = output_directory.join("cached-moments.ndjson");
    let interactions_path = output_directory.join("cached-moment-interactions.ndjson");
    let coverage_path = output_directory.join("cached-surfaces.json");
    let mut moments_writer = private_writer(&moments_path)?;
    let mut interactions_writer = private_writer(&interactions_path)?;
    let mut tables = Vec::new();
    let mut moment_count = 0_u64;
    let mut interaction_count = 0_u64;
    let mut semantic_gap_count = 0_u64;
    let mut source_database_present = false;

    for database in catalog.databases.iter().filter(|database| {
        let logical = database.logical_path.to_ascii_lowercase();
        logical == "sns/sns.db" || logical.ends_with("/sns.db") || logical == "sns.db"
    }) {
        source_database_present = true;
        let connection =
            Connection::open_with_flags(&database.path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        for table in &database.tables {
            let columns = table_columns(&connection, table)?;
            let source_table_id = opaque_id(table.as_bytes());
            let source_row_count = table_row_count(&connection, table)?;
            let (role, reason) = classify_table(table, &columns);
            let restored_row_count = match role {
                CachedSurfaceTableRole::MomentTimeline => restore_moments(
                    &connection,
                    table,
                    &columns,
                    &database.source_set_id,
                    &database.logical_path,
                    &source_table_id,
                    account_id,
                    &catalog.manifest.created_at,
                    &mut moments_writer,
                    &mut semantic_gap_count,
                )?,
                CachedSurfaceTableRole::MomentInteraction => restore_interactions(
                    &connection,
                    table,
                    &columns,
                    &database.source_set_id,
                    &database.logical_path,
                    &source_table_id,
                    account_id,
                    &catalog.manifest.created_at,
                    &mut interactions_writer,
                )?,
                CachedSurfaceTableRole::UnsupportedCandidate | CachedSurfaceTableRole::Other => 0,
            };
            match role {
                CachedSurfaceTableRole::MomentTimeline => moment_count += restored_row_count,
                CachedSurfaceTableRole::MomentInteraction => {
                    interaction_count += restored_row_count
                }
                CachedSurfaceTableRole::UnsupportedCandidate | CachedSurfaceTableRole::Other => {}
            }
            tables.push(CachedSurfaceTableCoverage {
                source_set_id: database.source_set_id.clone(),
                source_logical_path: database.logical_path.clone(),
                source_table_id,
                source_table_name: table.clone(),
                columns,
                source_row_count,
                restored_row_count,
                role,
                classification_reason: reason.to_string(),
            });
        }
    }
    moments_writer.flush()?;
    interactions_writer.flush()?;
    tables.sort_by(|left, right| {
        (
            &left.source_logical_path,
            &left.source_table_name,
            &left.source_set_id,
        )
            .cmp(&(
                &right.source_logical_path,
                &right.source_table_name,
                &right.source_set_id,
            ))
    });
    let coverage = CachedSurfaceCoverage {
        format_version: 1,
        observed_at: catalog.manifest.created_at.clone(),
        cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
        source_database_present,
        moment_count,
        interaction_count,
        semantic_gap_count,
        tables,
    };
    write_json(&coverage_path, &coverage)?;
    Ok(CachedSurfaceRestoration {
        moments_path,
        interactions_path,
        coverage_path,
        coverage,
    })
}

#[allow(clippy::too_many_arguments)]
fn restore_moments(
    connection: &Connection,
    table: &str,
    columns: &[String],
    source_set_id: &str,
    source_logical_path: &str,
    source_table_id: &str,
    account_id: &str,
    observed_at: &str,
    writer: &mut BufWriter<File>,
    semantic_gap_count: &mut u64,
) -> Result<u64, RestoreError> {
    let sql = format!(
        "SELECT rowid, * FROM {} ORDER BY rowid",
        quote_identifier(table)
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        let source_row_id = row_i64(row, 0).unwrap_or_default();
        let timeline_id = column_raw(row, columns, &["tid"]).unwrap_or(RawSQLiteValue::Null);
        let user_name = column_bytes(row, columns, &["user_name", "username"]);
        let content = column_bytes(row, columns, &["content"]);
        let pack_info = column_bytes(row, columns, &["pack_info_buf", "pack_info"]);
        let typed_content = content
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        let typed_pack = pack_info
            .as_deref()
            .map(String::from_utf8_lossy)
            .unwrap_or_default();
        let xml_author = tag_text(&typed_content, "username");
        let author = xml_author
            .as_deref()
            .map(str::as_bytes)
            .or(user_name.as_deref())
            .filter(|value| !value.is_empty());
        let has_timeline =
            typed_content.contains("<TimelineObject") || typed_content.contains("<timelineobject");
        let (semantic_decode_state, semantic_gap_reason) = if content.is_none() {
            *semantic_gap_count += 1;
            (
                SemanticDecodeState::Partial,
                Some("cached moment content is absent; raw columns were retained".to_string()),
            )
        } else if !has_timeline {
            *semantic_gap_count += 1;
            (
                SemanticDecodeState::Partial,
                Some(
                    "cached moment XML lacks a recognizable TimelineObject; raw content was retained"
                        .to_string(),
                ),
            )
        } else {
            (SemanticDecodeState::Complete, None)
        };
        let identity = format!("{source_set_id}:{source_table_id}:{source_row_id}");
        let moment = CanonicalCachedMoment {
            canonical_id: opaque_id(identity.as_bytes()),
            account_id: account_id.to_string(),
            source_set_id: source_set_id.to_string(),
            source_logical_path: source_logical_path.to_string(),
            source_table_id: source_table_id.to_string(),
            source_table_name: table.to_string(),
            source_row_id,
            timeline_id,
            author_id: author.map(|value| scoped_opaque_id(account_id, value)),
            author_source_identifier_base64: author.map(encode),
            created_at_unix: tag_i64(&typed_content, "createTime"),
            content_type: section(&typed_content, "ContentObject")
                .and_then(|value| tag_i64(value, "type")),
            content_description_base64: tag_text(&typed_content, "contentDesc")
                .as_deref()
                .map(|value| encode(value.as_bytes())),
            title_base64: section(&typed_content, "ContentObject")
                .and_then(|value| tag_text(value, "title"))
                .as_deref()
                .map(|value| encode(value.as_bytes())),
            description_base64: section(&typed_content, "ContentObject")
                .and_then(|value| tag_text(value, "description"))
                .as_deref()
                .map(|value| encode(value.as_bytes())),
            content_url_base64: section(&typed_content, "ContentObject")
                .and_then(|value| tag_text(value, "contentUrl"))
                .as_deref()
                .map(|value| encode(value.as_bytes())),
            media_count: section(&typed_content, "mediaList")
                .map(|value| count_open_tags(value, "media"))
                .unwrap_or_default(),
            like_count: section(&typed_pack, "like_user_list")
                .or_else(|| section(&typed_content, "like_user_list"))
                .map(|value| count_open_tags(value, "user_comment"))
                .unwrap_or_default(),
            comment_count: section(&typed_pack, "comment_user_list")
                .or_else(|| section(&typed_content, "comment_user_list"))
                .map(|value| count_open_tags(value, "user_comment"))
                .unwrap_or_default(),
            raw_content_base64: content.as_deref().map(encode),
            raw_pack_info_base64: pack_info.as_deref().map(encode),
            raw_columns: raw_columns(row, columns),
            semantic_decode_state,
            semantic_gap_reason,
            cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
            observed_at: observed_at.to_string(),
        };
        serde_json::to_writer(&mut *writer, &moment)?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

#[allow(clippy::too_many_arguments)]
fn restore_interactions(
    connection: &Connection,
    table: &str,
    columns: &[String],
    source_set_id: &str,
    source_logical_path: &str,
    source_table_id: &str,
    account_id: &str,
    observed_at: &str,
    writer: &mut BufWriter<File>,
) -> Result<u64, RestoreError> {
    let sql = format!(
        "SELECT rowid, * FROM {} ORDER BY rowid",
        quote_identifier(table)
    );
    let mut statement = connection.prepare(&sql)?;
    let mut rows = statement.query([])?;
    let mut count = 0_u64;
    while let Some(row) = rows.next()? {
        let source_row_id = row_i64(row, 0).unwrap_or_default();
        let raw_type = column_i64(row, columns, &["type"]);
        let from = column_bytes(row, columns, &["from_username"]).filter(|value| !value.is_empty());
        let to = column_bytes(row, columns, &["to_username"]).filter(|value| !value.is_empty());
        let identity = format!("{source_set_id}:{source_table_id}:{source_row_id}");
        let interaction = CanonicalCachedMomentInteraction {
            canonical_id: opaque_id(identity.as_bytes()),
            account_id: account_id.to_string(),
            source_set_id: source_set_id.to_string(),
            source_logical_path: source_logical_path.to_string(),
            source_table_id: source_table_id.to_string(),
            source_table_name: table.to_string(),
            source_row_id,
            local_id: column_i64(row, columns, &["local_id"]),
            feed_id: column_raw(row, columns, &["feed_id"]).unwrap_or(RawSQLiteValue::Null),
            created_at_unix: column_i64(row, columns, &["create_time"]),
            kind: match raw_type {
                Some(1) => CachedMomentInteractionKind::Comment,
                Some(2) => CachedMomentInteractionKind::Like,
                _ => CachedMomentInteractionKind::Unknown,
            },
            raw_type,
            from_participant_id: from
                .as_deref()
                .map(|value| scoped_opaque_id(account_id, value)),
            from_source_identifier_base64: from.as_deref().map(encode),
            from_nickname_base64: column_bytes(row, columns, &["from_nickname"])
                .as_deref()
                .map(encode),
            to_participant_id: to
                .as_deref()
                .map(|value| scoped_opaque_id(account_id, value)),
            to_source_identifier_base64: to.as_deref().map(encode),
            to_nickname_base64: column_bytes(row, columns, &["to_nickname"])
                .as_deref()
                .map(encode),
            content_base64: column_bytes(row, columns, &["content"])
                .as_deref()
                .map(encode),
            raw_columns: raw_columns(row, columns),
            cache_completeness: CachedSurfaceCompleteness::PartialLocalCache,
            observed_at: observed_at.to_string(),
        };
        serde_json::to_writer(&mut *writer, &interaction)?;
        writer.write_all(b"\n")?;
        count += 1;
    }
    Ok(count)
}

fn classify_table(table: &str, columns: &[String]) -> (CachedSurfaceTableRole, &'static str) {
    if table.eq_ignore_ascii_case("SnsTimeLine") {
        return if has_columns(columns, &["tid", "user_name", "content"]) {
            (
                CachedSurfaceTableRole::MomentTimeline,
                "matched the pinned SnsTimeLine identity and required columns",
            )
        } else {
            (
                CachedSurfaceTableRole::UnsupportedCandidate,
                "SnsTimeLine is missing one or more required columns",
            )
        };
    }
    if table.eq_ignore_ascii_case("SnsMessage_tmp3") {
        return if has_columns(
            columns,
            &[
                "local_id",
                "create_time",
                "type",
                "feed_id",
                "from_username",
                "to_username",
                "content",
            ],
        ) {
            (
                CachedSurfaceTableRole::MomentInteraction,
                "matched the pinned SnsMessage_tmp3 identity and required columns",
            )
        } else {
            (
                CachedSurfaceTableRole::UnsupportedCandidate,
                "SnsMessage_tmp3 is missing one or more required columns",
            )
        };
    }
    (
        CachedSurfaceTableRole::Other,
        "table is retained as SNS schema coverage but has no verified cached-surface adapter",
    )
}

fn has_columns(columns: &[String], required: &[&str]) -> bool {
    required.iter().all(|required| {
        columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(required))
    })
}

fn table_columns(connection: &Connection, table: &str) -> Result<Vec<String>, RestoreError> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    Ok(connection
        .prepare(&sql)?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?)
}

fn table_row_count(connection: &Connection, table: &str) -> Result<u64, RestoreError> {
    let sql = format!("SELECT count(*) FROM {}", quote_identifier(table));
    let value: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}

fn raw_columns(row: &Row<'_>, columns: &[String]) -> BTreeMap<String, RawSQLiteValue> {
    columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            (
                column.clone(),
                raw_value(row.get_ref(index + 1).unwrap_or(ValueRef::Null)),
            )
        })
        .collect()
}

fn column_index(columns: &[String], candidates: &[&str]) -> Option<usize> {
    columns.iter().position(|column| {
        candidates
            .iter()
            .any(|candidate| column.eq_ignore_ascii_case(candidate))
    })
}

fn column_raw(row: &Row<'_>, columns: &[String], names: &[&str]) -> Option<RawSQLiteValue> {
    let index = column_index(columns, names)?;
    Some(raw_value(row.get_ref(index + 1).ok()?))
}

fn column_bytes(row: &Row<'_>, columns: &[String], names: &[&str]) -> Option<Vec<u8>> {
    let index = column_index(columns, names)?;
    match row.get_ref(index + 1).ok()? {
        ValueRef::Blob(value) | ValueRef::Text(value) => Some(value.to_vec()),
        ValueRef::Integer(value) => Some(value.to_string().into_bytes()),
        ValueRef::Real(value) => Some(value.to_string().into_bytes()),
        ValueRef::Null => None,
    }
}

fn column_i64(row: &Row<'_>, columns: &[String], names: &[&str]) -> Option<i64> {
    let index = column_index(columns, names)?;
    row_i64(row, index + 1)
}

fn row_i64(row: &Row<'_>, index: usize) -> Option<i64> {
    match row.get_ref(index).ok()? {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        ValueRef::Blob(_) | ValueRef::Null => None,
    }
}

fn raw_value(value: ValueRef<'_>) -> RawSQLiteValue {
    match value {
        ValueRef::Null => RawSQLiteValue::Null,
        ValueRef::Integer(value) => RawSQLiteValue::Integer(value),
        ValueRef::Real(value) => RawSQLiteValue::Real(value),
        ValueRef::Text(value) => RawSQLiteValue::TextBase64(encode(value)),
        ValueRef::Blob(value) => RawSQLiteValue::BlobBase64(encode(value)),
    }
}

fn tag_i64(value: &str, tag: &str) -> Option<i64> {
    tag_text(value, tag)?.trim().parse().ok()
}

fn tag_text(value: &str, tag: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let open = format!("<{}>", tag.to_ascii_lowercase());
    let open_with_attributes = format!("<{} ", tag.to_ascii_lowercase());
    let start = lower
        .find(&open)
        .map(|index| index + open.len())
        .or_else(|| {
            lower
                .find(&open_with_attributes)
                .and_then(|index| lower[index..].find('>').map(|offset| index + offset + 1))
        })?;
    let close = format!("</{}>", tag.to_ascii_lowercase());
    let end = lower[start..].find(&close).map(|offset| start + offset)?;
    let mut result = value[start..end].trim();
    if result.starts_with("<![CDATA[") && result.ends_with("]]>") && result.len() >= 12 {
        result = &result[9..result.len() - 3];
    }
    Some(result.to_string())
}

fn section<'a>(value: &'a str, tag: &str) -> Option<&'a str> {
    let lower = value.to_ascii_lowercase();
    let open_prefix = format!("<{}", tag.to_ascii_lowercase());
    let open = lower.find(&open_prefix)?;
    let start = lower[open..].find('>').map(|offset| open + offset + 1)?;
    let close = format!("</{}>", tag.to_ascii_lowercase());
    let end = lower[start..].find(&close).map(|offset| start + offset)?;
    Some(&value[start..end])
}

fn count_open_tags(value: &str, tag: &str) -> u64 {
    let lower = value.to_ascii_lowercase();
    let prefix = format!("<{}", tag.to_ascii_lowercase());
    let bytes = lower.as_bytes();
    let mut start = 0;
    let mut count = 0_u64;
    while let Some(offset) = lower[start..].find(&prefix) {
        let index = start + offset + prefix.len();
        if bytes
            .get(index)
            .is_some_and(|value| matches!(*value, b'>' | b' ' | b'/' | b'\t' | b'\r' | b'\n'))
        {
            count += 1;
        }
        start = index;
    }
    count
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn opaque_id(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn encode(value: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}

fn private_writer(path: &Path) -> Result<BufWriter<File>, RestoreError> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    Ok(BufWriter::new(file))
}

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<(), RestoreError> {
    let mut writer = private_writer(path)?;
    serde_json::to_writer_pretty(&mut writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tolerant_xml_helpers_preserve_missing_and_messy_fields() {
        let value = "<TimelineObject><contentDesc><![CDATA[hello <world>]]></contentDesc><mediaList><media/><media id='2'/></mediaList></TimelineObject>";
        assert_eq!(
            tag_text(value, "contentDesc").as_deref(),
            Some("hello <world>")
        );
        assert_eq!(
            section(value, "mediaList").map(|value| count_open_tags(value, "media")),
            Some(2)
        );
        assert_eq!(tag_text(value, "missing"), None);
    }
}
