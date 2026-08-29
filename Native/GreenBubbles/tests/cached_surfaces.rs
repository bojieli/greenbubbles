use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use greenbubbles::tools::{
    create_tool_policy_with_cached_moments, CachedMomentField, CachedMomentsToolScope,
};
use greenbubbles::{
    audit::{audit_archive, audit_archive_with_progress},
    connector::{
        ConnectorDestination, ConnectorOperation, ConnectorRequest, ConnectorResult,
        ConnectorService, CONNECTOR_API_VERSION,
    },
    prepare_catalog,
    replica::{
        audit_replica, bootstrap_replica, get_replica_changes, replica_coverage, replica_status,
        search_replica_cached_moments, synchronize_replica, ReplicaCachedMomentFilter,
        ReplicaCachedSurfaceAvailability,
    },
    restore_catalog, CanonicalCachedMoment, ProgressEvent, ProgressObserver, ProgressPhase,
    ProgressState, ReplicaKey, RestorationOptions, RestorationReport, SemanticDecodeState,
    SnapshotEntry, SnapshotFileRole, SnapshotManifest,
};
use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

#[test]
fn restores_cached_moments_and_interactions_without_claiming_cache_completeness() {
    let fixture = tempfile::tempdir().unwrap();
    let snapshot = fixture.path().join("snapshot");
    fs::create_dir_all(snapshot.join("sets/0000")).unwrap();
    let database = snapshot.join("sets/0000/database.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            r#"CREATE TABLE SnsTimeLine(
               tid INTEGER, user_name TEXT, content BLOB, pack_info_buf BLOB, opaque BLOB
             );
             INSERT INTO SnsTimeLine VALUES(
               1001, 'fallback-author',
               '<SnsDataItem><TimelineObject><id>sns-1001</id><username>wxid_author</username><createTime>1774857283</createTime><contentDesc><![CDATA[hello <world>]]></contentDesc><ContentObject><type>6</type><title>Article</title><description>Details</description><contentUrl>https://example.test/article</contentUrl><mediaList><media/><media id="2"/></mediaList></ContentObject></TimelineObject></SnsDataItem>',
               '<LocalExtraInfo><like_user_list><user_comment/><user_comment/></like_user_list><comment_user_list><user_comment/></comment_user_list></LocalExtraInfo>',
               x'00ff'
             );
             INSERT INTO SnsTimeLine VALUES(1002, 'wxid_partial', NULL, NULL, x'01');
             CREATE TABLE SnsMessage_tmp3(
               local_id INTEGER, create_time INTEGER, type INTEGER, feed_id INTEGER,
               from_username TEXT, from_nickname TEXT, to_username TEXT,
               to_nickname TEXT, content BLOB
             );
             INSERT INTO SnsMessage_tmp3 VALUES(
               7, 1774857290, 1, 1001, 'wxid_commenter', 'Commenter',
               'wxid_author', 'Author', x'6869'
             );
             INSERT INTO SnsMessage_tmp3 VALUES(
               8, 1774857291, 2, 1001, 'wxid_liker', 'Liker',
               'wxid_author', 'Author', NULL
             );
             CREATE TABLE SnsTimeLineLegacy(tid INTEGER, content BLOB);
             INSERT INTO SnsTimeLineLegacy VALUES(1, x'00');
             CREATE TABLE SnsConfig(key TEXT, value BLOB);"#,
        )
        .unwrap();
    drop(connection);

    let bytes = fs::read(&database).unwrap();
    let metadata = fs::metadata(&database).unwrap();
    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000099".to_string(),
        created_at: "2026-08-27T03:04:05Z".to_string(),
        source_fingerprint: "cached-surface-fixture".to_string(),
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: "source".to_string(),
                path: None,
            },
            source_set_id: "sns-set".to_string(),
            logical_path: "sns/sns.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint: greenbubbles::manifest::SourceFileFingerprint {
                device_id: 1,
                file_id: 2,
                byte_count: metadata.len() as i64,
                modified_seconds: 0,
                modified_nanoseconds: 0,
            },
            sha256: hex::encode(Sha256::digest(bytes)),
        }],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let output = fixture.path().join("archive");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: output.clone(),
            account_root: None,
            defer_media: true,
        },
    )
    .unwrap();
    assert_eq!(report.integrity.cached_moment_count, 2);
    assert_eq!(report.integrity.cached_moment_interaction_count, 2);
    assert_eq!(report.integrity.cached_surface_semantic_gap_count, 1);
    assert!(report.cached_moments_path.is_some());
    assert!(report.cached_moment_interactions_path.is_some());
    assert!(report.cached_surfaces_path.is_some());

    let moments = ndjson(&output.join("cached-moments.ndjson"));
    assert_eq!(moments.len(), 2);
    assert_eq!(moments[0]["createdAtUnix"], json!(1774857283));
    assert_eq!(moments[0]["contentType"], json!(6));
    assert_eq!(moments[0]["mediaCount"], json!(2));
    assert_eq!(moments[0]["likeCount"], json!(2));
    assert_eq!(moments[0]["commentCount"], json!(1));
    assert_eq!(moments[0]["cacheCompleteness"], json!("partialLocalCache"));
    assert_eq!(moments[0]["observedAt"], json!("2026-08-27T03:04:05Z"));
    assert_eq!(
        moments[0]["rawColumns"]["opaque"],
        json!({
            "storageClass": "blobBase64", "value": "AP8="
        })
    );
    assert_eq!(moments[1]["semanticDecodeState"], json!("partial"));

    let interactions = ndjson(&output.join("cached-moment-interactions.ndjson"));
    assert_eq!(interactions.len(), 2);
    assert_eq!(interactions[0]["kind"], json!("comment"));
    assert_eq!(interactions[0]["contentBase64"], json!("aGk="));
    assert_eq!(interactions[1]["kind"], json!("like"));

    let coverage: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("cached-surfaces.json")).unwrap()).unwrap();
    assert_eq!(coverage["formatVersion"], json!(2));
    assert_eq!(
        coverage["schemaProfileFingerprint"].as_str().unwrap().len(),
        64
    );
    assert_eq!(coverage["cacheCompleteness"], json!("partialLocalCache"));
    assert_eq!(coverage["sourceDatabasePresent"], json!(true));
    assert_eq!(coverage["momentCount"], json!(2));
    assert_eq!(coverage["interactionCount"], json!(2));
    assert_eq!(coverage["semanticGapCount"], json!(1));
    assert!(coverage["tables"].as_array().unwrap().iter().any(|table| {
        table["sourceTableName"] == "SnsTimeLine" && table["role"] == "momentTimeline"
    }));
    assert!(coverage["tables"].as_array().unwrap().iter().any(|table| {
        table["sourceTableName"] == "SnsTimeLineLegacy" && table["role"] == "other"
    }));
    assert!(coverage["tables"].as_array().unwrap().iter().all(|table| {
        table["schemaFingerprint"]
            .as_str()
            .is_some_and(|fingerprint| fingerprint.len() == 64)
    }));
    for name in [
        "cached-moments.ndjson",
        "cached-moment-interactions.ndjson",
        "cached-surfaces.json",
    ] {
        assert_eq!(
            fs::metadata(output.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    let audit_progress = CapturingProgress::default();
    let archive_audit = audit_archive_with_progress(&output, &audit_progress).unwrap();
    assert_eq!(archive_audit.cached_moment_count, 2);
    assert_eq!(archive_audit.cached_moment_interaction_count, 2);
    assert!(archive_audit.report_matches_archive);
    assert!(audit_progress.events.lock().unwrap().iter().any(|event| {
        event.phase == ProgressPhase::ArchiveAudit
            && event.state == ProgressState::Completed
            && event.operation == "auditArchive"
            && event.restored_record_count == Some(4)
    }));

    let mut identity_tampered = moments.clone();
    identity_tampered[0]["canonicalId"] = json!("substituted-cached-identity");
    write_ndjson(&output.join("cached-moments.ndjson"), &identity_tampered);
    assert!(audit_archive(&output)
        .unwrap_err()
        .to_string()
        .contains("canonical identity is not source-deterministic"));
    write_ndjson(&output.join("cached-moments.ndjson"), &moments);
    assert!(audit_archive(&output).is_ok());

    let replica_directory = fixture.path().join("replica-private");
    fs::create_dir(&replica_directory).unwrap();
    fs::set_permissions(&replica_directory, fs::Permissions::from_mode(0o700)).unwrap();
    let replica = replica_directory.join("replica.db");
    let key = ReplicaKey::from_bytes([0x5a; 32]);
    let bootstrapped = bootstrap_replica(&output, &replica, &key).unwrap();
    assert_eq!(bootstrapped.cached_moment_count, 2);
    assert_eq!(bootstrapped.cached_moment_interaction_count, 2);
    let first_page = search_replica_cached_moments(
        &replica,
        &key,
        &ReplicaCachedMomentFilter::default(),
        None,
        1,
    )
    .unwrap();
    assert_eq!(
        first_page.availability,
        ReplicaCachedSurfaceAvailability::Available
    );
    assert_eq!(
        first_page.observed_at.as_deref(),
        Some("2026-08-27T03:04:05Z")
    );
    assert_eq!(first_page.items.len(), 1);
    let stale_cursor = first_page.next_cursor.unwrap();
    let author = first_page.items[0].author_id.clone().unwrap();
    let author_page = search_replica_cached_moments(
        &replica,
        &key,
        &ReplicaCachedMomentFilter {
            author_id: Some(author),
            ..Default::default()
        },
        None,
        10,
    )
    .unwrap();
    assert_eq!(author_page.items.len(), 1);
    assert_eq!(
        replica_status(&replica, &key).unwrap().cached_moment_count,
        2
    );
    let replica_audit = audit_replica(&replica, &key).unwrap();
    assert_eq!(replica_audit.cached_moment_count, 2);
    assert_eq!(replica_audit.cached_moment_interaction_count, 2);
    assert!(
        replica_coverage(&replica, &key)
            .unwrap()
            .cached_surfaces
            .unwrap()
            .source_database_present
    );

    let mut canonical = read_ndjson::<CanonicalCachedMoment>(&output.join("cached-moments.ndjson"));
    canonical.remove(0);
    canonical[0].title_base64 = Some("dXBkYXRlZA==".to_string());
    canonical[0].created_at_unix = Some(1_774_857_291);
    let mut added = canonical[0].clone();
    added.source_row_id = 1003;
    added.canonical_id = hex::encode(Sha256::digest(
        format!(
            "{}:{}:{}",
            added.source_set_id, added.source_table_id, added.source_row_id
        )
        .as_bytes(),
    ));
    added.created_at_unix = Some(1_774_857_292);
    canonical.push(added);
    write_ndjson(&output.join("cached-moments.ndjson"), &canonical);
    let semantic_gap_count = canonical
        .iter()
        .filter(|moment| moment.semantic_decode_state != SemanticDecodeState::Complete)
        .count() as u64;
    let mut changed_cached_coverage: serde_json::Value =
        serde_json::from_slice(&fs::read(output.join("cached-surfaces.json")).unwrap()).unwrap();
    changed_cached_coverage["semanticGapCount"] = json!(semantic_gap_count);
    fs::write(
        output.join("cached-surfaces.json"),
        serde_json::to_vec_pretty(&changed_cached_coverage).unwrap(),
    )
    .unwrap();
    let mut changed_report: RestorationReport =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    changed_report.source_fingerprint = "cached-surface-fixture-2".to_string();
    changed_report.integrity.cached_surface_semantic_gap_count = semantic_gap_count;
    write_report_with_refreshed_storage(&output, &mut changed_report);
    let synchronized = synchronize_replica(&output, &replica, &key).unwrap();
    assert_eq!(synchronized.added_count, 1);
    assert_eq!(synchronized.changed_count, 1);
    assert_eq!(synchronized.removed_count, 1);
    assert!(search_replica_cached_moments(
        &replica,
        &key,
        &ReplicaCachedMomentFilter::default(),
        Some(&stale_cursor),
        1,
    )
    .is_err());
    let changes = get_replica_changes(&replica, &key, None, 100).unwrap();
    assert!(changes
        .items
        .iter()
        .any(|change| change.entity_kind == "cachedMoment"));

    let policy = replica_directory.join("cached-policy.json");
    create_tool_policy_with_cached_moments(
        &output,
        &policy,
        BTreeMap::new(),
        Some(CachedMomentsToolScope {
            fields: BTreeSet::from([
                CachedMomentField::CreatedAt,
                CachedMomentField::ContentDescription,
                CachedMomentField::Title,
                CachedMomentField::LikeCount,
            ]),
            not_before_unix: Some(1_774_857_280),
            not_after_unix: Some(1_774_857_300),
            allow_remote_model: false,
        }),
        1,
        4_096,
        1_024,
    )
    .unwrap();
    let drafts = replica_directory.join("drafts");
    fs::create_dir(&drafts).unwrap();
    fs::set_permissions(&drafts, fs::Permissions::from_mode(0o700)).unwrap();
    let service = ConnectorService::open(
        &replica,
        &key,
        &policy,
        &replica_directory.join("audit.ndjson"),
        &drafts,
    )
    .unwrap();
    let capabilities = service.handle(connector_request(
        "cached-capabilities",
        ConnectorDestination::Local,
        ConnectorOperation::Capabilities,
    ));
    let ConnectorResult::Capabilities(capabilities) = capabilities.result.unwrap() else {
        panic!("unexpected capabilities result")
    };
    assert!(capabilities.cached_moments_read.enabled);
    assert!(!capabilities.authenticated_active_read.available);
    let cached = service.handle(connector_request(
        "cached-local",
        ConnectorDestination::Local,
        ConnectorOperation::GetCachedMoments {
            author_id: None,
            not_before_unix: None,
            not_after_unix: None,
            content_type: None,
            cursor: None,
            limit: Some(50),
        },
    ));
    assert!(cached.ok);
    let ConnectorResult::CachedMoments(cached) = cached.result.unwrap() else {
        panic!("unexpected cached Moments result")
    };
    assert_eq!(cached.moments.len(), 1);
    assert_eq!(cached.moments[0].title.as_deref(), Some("updated"));
    assert!(cached.moments[0].author_id.is_none());
    let minimized_json = serde_json::to_value(&cached).unwrap();
    assert!(minimized_json["moments"][0].get("rawColumns").is_none());
    assert!(minimized_json["moments"][0]
        .get("rawContentBase64")
        .is_none());
    let remote = service.handle(connector_request(
        "cached-remote-denied",
        ConnectorDestination::RemoteModel,
        ConnectorOperation::GetCachedMoments {
            author_id: None,
            not_before_unix: None,
            not_after_unix: None,
            content_type: None,
            cursor: None,
            limit: Some(1),
        },
    ));
    assert!(!remote.ok);
    let outside_time = service.handle(connector_request(
        "cached-outside-time",
        ConnectorDestination::Local,
        ConnectorOperation::GetCachedMoments {
            author_id: None,
            not_before_unix: Some(2_000_000_000),
            not_after_unix: None,
            content_type: None,
            cursor: None,
            limit: Some(1),
        },
    ));
    assert!(!outside_time.ok);
    for index in 0..59 {
        let allowed = service.handle(connector_request(
            &format!("cached-rate-allowed-{index}"),
            ConnectorDestination::Local,
            ConnectorOperation::GetCachedMoments {
                author_id: None,
                not_before_unix: None,
                not_after_unix: None,
                content_type: None,
                cursor: None,
                limit: Some(1),
            },
        ));
        assert!(allowed.ok);
    }
    let rate_limited = service.handle(connector_request(
        "cached-rate-denied",
        ConnectorDestination::Local,
        ConnectorOperation::GetCachedMoments {
            author_id: None,
            not_before_unix: None,
            not_after_unix: None,
            content_type: None,
            cursor: None,
            limit: Some(1),
        },
    ));
    assert!(!rate_limited.ok);
    assert!(rate_limited
        .error
        .unwrap()
        .message
        .contains("limited to 60 requests per rolling minute"));

    fs::remove_file(output.join("cached-moment-interactions.ndjson")).unwrap();
    assert!(bootstrap_replica(
        &output,
        &replica_directory.join("partial-cached-replica.db"),
        &key,
    )
    .is_err());
}

#[test]
fn unreadable_cached_table_rows_are_omitted_without_aborting_the_archive_or_replica() {
    let fixture = tempfile::tempdir().unwrap();
    let snapshot = fixture.path().join("snapshot-unreadable-cached-table");
    fs::create_dir_all(snapshot.join("sets/0000")).unwrap();
    let database = snapshot.join("sets/0000/database.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            r#"CREATE TABLE SnsTimeLine(
                 tid INTEGER PRIMARY KEY,
                 user_name TEXT,
                 content BLOB,
                 pack_info_buf BLOB
               ) WITHOUT ROWID;
               INSERT INTO SnsTimeLine VALUES(
                 1,
                 'wxid_author',
                 '<TimelineObject><username>wxid_author</username><createTime>1</createTime></TimelineObject>',
                 NULL
               );"#,
        )
        .unwrap();
    drop(connection);

    let bytes = fs::read(&database).unwrap();
    let metadata = fs::metadata(&database).unwrap();
    let manifest = SnapshotManifest {
        manifest_format_version: 1,
        snapshot_id: "00000000-0000-4000-8000-000000000098".to_string(),
        created_at: "2026-08-27T03:04:05Z".to_string(),
        source_fingerprint: "unreadable-cached-table-fixture".to_string(),
        account_binding: None,
        client_build: None,
        acquisition: None,
        entries: vec![SnapshotEntry {
            source: greenbubbles::manifest::PathReference {
                opaque_id: "source".to_string(),
                path: None,
            },
            source_set_id: "sns-unreadable-set".to_string(),
            logical_path: "sns/sns.db".to_string(),
            relative_path: "sets/0000/database.db".to_string(),
            role: SnapshotFileRole::Database,
            fingerprint: greenbubbles::manifest::SourceFileFingerprint {
                device_id: 1,
                file_id: 3,
                byte_count: metadata.len() as i64,
                modified_seconds: 0,
                modified_nanoseconds: 0,
            },
            sha256: hex::encode(Sha256::digest(bytes)),
        }],
    };
    fs::write(
        snapshot.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    let catalog = prepare_catalog(&snapshot, None).unwrap();
    let archive = fixture.path().join("archive-unreadable-cached-table");
    let report = restore_catalog(
        &catalog,
        &RestorationOptions {
            output_directory: archive.clone(),
            account_root: None,
            defer_media: true,
        },
    )
    .unwrap();
    assert_eq!(report.integrity.cached_moment_count, 0);
    assert_eq!(report.integrity.cached_surface_omitted_row_count, 1);
    assert!(!report.completion.full_restoration_achieved);

    let coverage: serde_json::Value =
        serde_json::from_slice(&fs::read(archive.join("cached-surfaces.json")).unwrap()).unwrap();
    assert_eq!(coverage["omittedRowCount"], json!(1));
    assert!(coverage["limitationCodes"]
        .as_array()
        .unwrap()
        .contains(&json!("cachedSurfaceRowsOmitted")));
    assert_eq!(coverage["tables"][0]["availability"], json!("unavailable"));
    assert_eq!(
        coverage["tables"][0]["limitationCode"],
        json!("unreadableCachedSurfaceRowsOmitted")
    );
    let archive_audit = audit_archive(&archive).unwrap();
    assert_eq!(archive_audit.cached_surface_omitted_row_count, 1);

    let private = fixture.path().join("replica-unreadable-cached-table");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    let replica = private.join("replica.db");
    let key = ReplicaKey::from_bytes([0x6a; 32]);
    bootstrap_replica(&archive, &replica, &key).unwrap();
    let status = replica_status(&replica, &key).unwrap();
    assert_eq!(status.cached_surface_omitted_row_count, Some(1));
    assert!(status
        .limitation_codes
        .contains(&"cachedSurfaceSourceRowsOmitted".to_string()));
    let page = search_replica_cached_moments(
        &replica,
        &key,
        &ReplicaCachedMomentFilter::default(),
        None,
        10,
    )
    .unwrap();
    assert_eq!(
        page.availability,
        ReplicaCachedSurfaceAvailability::Unavailable
    );
    assert!(page.items.is_empty());
    assert!(page
        .limitation_codes
        .contains(&"cachedSurfaceSourceRowsOmitted".to_string()));
}

#[derive(Default)]
struct CapturingProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl ProgressObserver for CapturingProgress {
    fn observe(&self, event: ProgressEvent) {
        self.events.lock().unwrap().push(event);
    }
}

fn connector_request(
    request_id: &str,
    destination: ConnectorDestination,
    operation: ConnectorOperation,
) -> ConnectorRequest {
    ConnectorRequest {
        api_version: CONNECTOR_API_VERSION.to_string(),
        request_id: request_id.to_string(),
        requester_id: "cached-surface-test".to_string(),
        destination,
        operation,
    }
}

fn ndjson(path: &std::path::Path) -> Vec<serde_json::Value> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn read_ndjson<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Vec<T> {
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn write_ndjson<T: serde::Serialize>(path: &std::path::Path, values: &[T]) {
    let mut bytes = Vec::new();
    for value in values {
        serde_json::to_writer(&mut bytes, value).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn write_report_with_refreshed_storage(archive: &std::path::Path, report: &mut RestorationReport) {
    let report_path = archive.join("report.json");
    let other_bytes = WalkDir::new(archive)
        .into_iter()
        .map(Result::unwrap)
        .filter(|entry| entry.file_type().is_file() && entry.path() != report_path)
        .map(|entry| entry.metadata().unwrap().len())
        .sum::<u64>();
    for _ in 0..8 {
        let mut bytes = serde_json::to_vec_pretty(report).unwrap();
        bytes.push(b'\n');
        let exact = other_bytes.saturating_add(bytes.len() as u64);
        let storage = report.storage.as_mut().unwrap();
        if storage.actual_archive_byte_count == exact {
            fs::write(&report_path, bytes).unwrap();
            return;
        }
        storage.actual_archive_byte_count = exact;
    }
    panic!("synthetic restoration report size did not converge");
}
