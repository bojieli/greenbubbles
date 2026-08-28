use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgressPhase {
    SnapshotVerification,
    KeyValidation,
    DatabasePreparation,
    RecordPlanning,
    RecordRestoration,
    ArchiveFinalization,
    ArchiveAudit,
    ReplicaApplication,
    ContextExport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgressState {
    Planned,
    Started,
    Advanced,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProgressUnit {
    Bytes,
    Records,
    Items,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub format_version: u32,
    pub privacy_safe: bool,
    pub phase: ProgressPhase,
    pub state: ProgressState,
    pub operation: String,
    pub unit: ProgressUnit,
    pub completed: u64,
    pub total: u64,
    pub phase_completed: u64,
    pub phase_total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_completed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_phase_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_phase_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_completed_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_archive_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_staging_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub estimated_peak_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_free_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_free_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staging_file_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_uncompressed_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staged_compressed_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_archive_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replica_file_byte_count: Option<u64>,
    #[serde(rename = "sourceSetID", skip_serializing_if = "Option::is_none")]
    pub source_set_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_key_match_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_unlock_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_database_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_database_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_ahead_log_byte_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_ahead_log_frame_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_columns: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_schema_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub table_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_table_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restored_record_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_record_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_record_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_gap_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_milliseconds: Option<u64>,
}

impl ProgressEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        phase: ProgressPhase,
        state: ProgressState,
        operation: impl Into<String>,
        unit: ProgressUnit,
        completed: u64,
        total: u64,
        phase_completed: u64,
        phase_total: u64,
    ) -> Self {
        Self {
            format_version: 3,
            privacy_safe: true,
            phase,
            state,
            operation: operation.into(),
            unit,
            completed,
            total,
            phase_completed,
            phase_total,
            workflow_completed: None,
            workflow_total: None,
            workflow_phase_index: None,
            workflow_phase_count: None,
            database_index: None,
            database_count: None,
            file_index: None,
            file_count: None,
            file_completed_byte_count: None,
            file_byte_count: None,
            source_byte_count: None,
            estimated_archive_byte_count: None,
            estimated_staging_byte_count: None,
            estimated_peak_byte_count: None,
            required_free_byte_count: None,
            available_free_byte_count: None,
            staging_file_byte_count: None,
            staged_uncompressed_byte_count: None,
            staged_compressed_byte_count: None,
            published_archive_byte_count: None,
            archive_byte_count: None,
            replica_file_byte_count: None,
            source_set_id: None,
            logical_path: None,
            storage_family: None,
            database_key_match_method: None,
            database_unlock_state: None,
            available_database_count: None,
            unavailable_database_count: None,
            database_byte_count: None,
            write_ahead_log_byte_count: None,
            write_ahead_log_frame_count: None,
            table_name: None,
            table_role: None,
            table_columns: None,
            table_schema_fingerprint: None,
            table_count: None,
            message_table_count: None,
            restored_record_count: None,
            source_record_count: None,
            rejected_record_count: None,
            semantic_gap_count: None,
            elapsed_milliseconds: None,
        }
    }

    /// Attach a monotonic, end-to-end workflow position to a phase-local event.
    ///
    /// Each phase contributes an equal, fixed amount. This is deliberately a
    /// workflow-stage percentage rather than an estimate of remaining wall
    /// time: byte and row percentages remain available in `phase_*` and
    /// `completed`/`total` respectively.
    pub fn attach_workflow(&mut self, phases: &[ProgressPhase]) {
        const PHASE_RESOLUTION: u64 = 1_000_000;

        let Some(phase_offset) = phases.iter().position(|phase| *phase == self.phase) else {
            return;
        };
        let phase_count = phases.len();
        if phase_count == 0 {
            return;
        }
        let within_phase = if self.phase_total > 0 {
            self.phase_completed.min(self.phase_total) as u128 * PHASE_RESOLUTION as u128
                / self.phase_total as u128
        } else if self.state == ProgressState::Completed {
            PHASE_RESOLUTION as u128
        } else {
            0
        } as u64;
        let completed = (phase_offset as u64)
            .saturating_mul(PHASE_RESOLUTION)
            .saturating_add(within_phase);
        let total = (phase_count as u64).saturating_mul(PHASE_RESOLUTION);

        self.workflow_completed = Some(completed.min(total));
        self.workflow_total = Some(total);
        self.workflow_phase_index = Some(phase_offset + 1);
        self.workflow_phase_count = Some(phase_count);
    }
}

pub trait ProgressObserver: Send + Sync {
    fn observe(&self, event: ProgressEvent);
}

pub struct NoProgress;

impl ProgressObserver for NoProgress {
    fn observe(&self, _event: ProgressEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_progress_is_monotonic_across_zero_and_nonzero_phases() {
        let phases = [
            ProgressPhase::SnapshotVerification,
            ProgressPhase::RecordRestoration,
            ProgressPhase::ArchiveFinalization,
        ];
        let mut halfway = ProgressEvent::new(
            ProgressPhase::SnapshotVerification,
            ProgressState::Advanced,
            "verifySnapshot",
            ProgressUnit::Bytes,
            5,
            10,
            5,
            10,
        );
        halfway.attach_workflow(&phases);
        assert_eq!(halfway.workflow_completed, Some(500_000));
        assert_eq!(halfway.workflow_total, Some(3_000_000));
        assert_eq!(halfway.workflow_phase_index, Some(1));

        let mut zero_rows_done = ProgressEvent::new(
            ProgressPhase::RecordRestoration,
            ProgressState::Completed,
            "restoreDatabaseRecords",
            ProgressUnit::Records,
            0,
            0,
            0,
            0,
        );
        zero_rows_done.attach_workflow(&phases);
        assert_eq!(zero_rows_done.workflow_completed, Some(2_000_000));

        let mut finished = ProgressEvent::new(
            ProgressPhase::ArchiveFinalization,
            ProgressState::Completed,
            "finalizeArchive",
            ProgressUnit::Items,
            0,
            0,
            0,
            0,
        );
        finished.attach_workflow(&phases);
        assert_eq!(finished.workflow_completed, finished.workflow_total);
    }
}
