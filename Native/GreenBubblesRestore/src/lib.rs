pub mod acquisition_audit;
pub mod action;
pub mod ai_context;
pub mod ai_memory;
pub mod archive;
pub mod artifact;
pub mod audit;
pub mod benchmark;
pub mod cached;
pub mod catalog;
pub mod connector;
pub mod database_keys;
pub mod diagnostic;
pub mod direct_connector;
pub mod entities;
pub mod error;
pub mod follow;
pub mod latency;
pub mod live_attachment;
pub mod live_query;
pub mod manifest;
pub mod merge;
pub mod model;
mod nested_xml;
pub mod operator;
pub mod progress;
pub mod query_profile;
pub mod reconcile;
pub mod recoverable_snapshot;
pub mod replica;
pub mod restore;
mod schema;
pub mod secret;
pub mod send_adapter;
pub mod send_contract;
pub mod send_outbox;
pub mod send_profile;
pub mod snapshot_protector;
pub mod tools;
pub mod transport;
mod wal;

pub use catalog::{
    preflight_snapshot, preflight_snapshot_with_progress, prepare_available_catalog_with_progress,
    prepare_catalog, prepare_catalog_batch_with_progress, prepare_catalog_with_progress,
    prepare_catalog_with_unlock, AvailableDatabaseSelection, DatabaseUnlockMaterial,
    DiagnosticDatabaseBatch, PreparedCatalog, PreparedDatabase, SnapshotStoragePreflightDatabase,
    SnapshotStoragePreflightReport, StorageFamily, UnavailableDatabase,
};
pub use database_keys::DatabaseKeySet;
pub use error::RestoreError;
pub use manifest::{
    ClientBuildCompatibilityEvidence, ClientBuildCompatibilityState, ClientBuildFingerprint,
    SnapshotAccountBinding, SnapshotAccountBindingEvidence, SnapshotAcquisitionEvidence,
    SnapshotAcquisitionMode, SnapshotEntry, SnapshotFileRole, SnapshotManifest,
    SnapshotSourceFileInventory, SnapshotSourceSetInventory,
};
pub use model::*;
pub use progress::{
    NoProgress, ProgressEvent, ProgressObserver, ProgressPhase, ProgressState, ProgressUnit,
};
pub use restore::{restore_catalog, restore_catalog_with_progress, RestorationOptions};
pub use secret::{DatabasePassphrase, ReplicaKey, SnapshotKey};
