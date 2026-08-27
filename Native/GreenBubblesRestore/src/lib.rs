pub mod acquisition_audit;
pub mod action;
pub mod archive;
pub mod artifact;
pub mod audit;
pub mod benchmark;
pub mod cached;
pub mod catalog;
pub mod connector;
pub mod entities;
pub mod error;
pub mod follow;
pub mod latency;
pub mod manifest;
pub mod merge;
pub mod model;
mod nested_xml;
pub mod operator;
pub mod reconcile;
pub mod replica;
pub mod restore;
mod schema;
pub mod secret;
pub mod tools;
pub mod transport;

pub use catalog::{
    preflight_snapshot, prepare_catalog, PreparedCatalog, PreparedDatabase,
    SnapshotStoragePreflightDatabase, SnapshotStoragePreflightReport, StorageFamily,
};
pub use error::RestoreError;
pub use manifest::{
    ClientBuildCompatibilityEvidence, ClientBuildCompatibilityState, ClientBuildFingerprint,
    SnapshotAcquisitionEvidence, SnapshotAcquisitionMode, SnapshotEntry, SnapshotFileRole,
    SnapshotManifest, SnapshotSourceFileInventory, SnapshotSourceSetInventory,
};
pub use model::*;
pub use restore::{restore_catalog, RestorationOptions};
pub use secret::{DatabasePassphrase, ReplicaKey};
