use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const ACTION_SAFETY_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionCapability {
    TextSend,
    ReplySend,
    FileSend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionLifecycleState {
    Drafted,
    Approved,
    Attempted,
    ObservedSent,
    ObservedFailed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionGuardDenial {
    InvalidContract,
    AcquisitionGateNotPassed,
    RestorationGateNotPassed,
    MechanismNotApproved,
    LegalReviewNotApproved,
    KillSwitchEngaged,
    AdapterMismatch,
    ClientBuildMismatch,
    AccountNotAllowed,
    ConversationNotAllowed,
    CapabilityNotAllowed,
    ApprovalMalformed,
    ApprovalBindingMismatch,
    ApprovalNotYetValid,
    ApprovalExpired,
    ApprovalAlreadyConsumed,
    IdempotencyKeyMalformed,
    IdempotencyKeyAlreadyReserved,
    RateLimitInvalid,
    RateLimitExceeded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionAdapterBinding {
    pub adapter_id: String,
    pub adapter_version: String,
    pub client_build_profile_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionGateEvidence {
    pub gate_decision_id: String,
    pub acquisition_gate_passed: bool,
    pub restoration_gate_passed: bool,
    pub mechanism_approved: bool,
    pub legal_review_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionAllowList {
    pub account_ids: BTreeSet<String>,
    pub conversation_ids: BTreeSet<String>,
    pub capabilities: BTreeSet<ActionCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionRateState {
    pub window_started_at_unix_nanoseconds: u128,
    pub window_ends_at_unix_nanoseconds: u128,
    pub maximum_attempts: u64,
    pub reserved_attempts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExternalApprovalEvidence {
    pub approval_id: String,
    pub immutable_binding_sha256: String,
    pub approver_id: String,
    pub approved_at_unix_nanoseconds: u128,
    pub expires_at_unix_nanoseconds: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionAttemptIntent {
    pub capability: ActionCapability,
    pub draft_id: String,
    pub account_id: String,
    pub conversation_id: String,
    pub adapter: ActionAdapterBinding,
    pub idempotency_key: String,
    pub approval: ExternalApprovalEvidence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionGuardContext {
    pub format_version: u32,
    pub now_unix_nanoseconds: u128,
    pub global_kill_switch_engaged: bool,
    pub gate: ActionGateEvidence,
    pub required_adapter: ActionAdapterBinding,
    pub allow_list: ActionAllowList,
    pub rate: ActionRateState,
    pub consumed_approval_ids: BTreeSet<String>,
    pub reserved_idempotency_keys: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionGuardDecision {
    pub permitted: bool,
    pub denials: BTreeSet<ActionGuardDenial>,
    pub approval_binding_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ActionLifecycleEvent {
    pub action_id: String,
    pub draft_id: String,
    pub approval_id: String,
    pub idempotency_key: String,
    pub state: ActionLifecycleState,
    pub observed_at_unix_nanoseconds: u128,
}

pub fn expected_approval_binding(
    gate_decision_id: &str,
    intent: &ActionAttemptIntent,
) -> Option<String> {
    if !is_sha256(gate_decision_id)
        || !is_sha256(&intent.draft_id)
        || intent.account_id.is_empty()
        || intent.conversation_id.is_empty()
        || intent.adapter.adapter_id.is_empty()
        || intent.adapter.adapter_version.is_empty()
        || intent.adapter.client_build_profile_id.is_empty()
    {
        return None;
    }
    let mut hasher = Sha256::new();
    for value in [
        gate_decision_id,
        action_capability_name(intent.capability),
        intent.draft_id.as_str(),
        intent.account_id.as_str(),
        intent.conversation_id.as_str(),
        intent.adapter.adapter_id.as_str(),
        intent.adapter.adapter_version.as_str(),
        intent.adapter.client_build_profile_id.as_str(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    Some(hex::encode(hasher.finalize()))
}

pub fn assess_action_attempt(
    context: &ActionGuardContext,
    intent: &ActionAttemptIntent,
) -> ActionGuardDecision {
    let mut denials = BTreeSet::new();
    if context.format_version != ACTION_SAFETY_CONTRACT_VERSION
        || !is_sha256(&context.gate.gate_decision_id)
        || context.required_adapter.adapter_id.is_empty()
        || context.required_adapter.adapter_version.is_empty()
        || context.required_adapter.client_build_profile_id.is_empty()
    {
        denials.insert(ActionGuardDenial::InvalidContract);
    }
    if !context.gate.acquisition_gate_passed {
        denials.insert(ActionGuardDenial::AcquisitionGateNotPassed);
    }
    if !context.gate.restoration_gate_passed {
        denials.insert(ActionGuardDenial::RestorationGateNotPassed);
    }
    if !context.gate.mechanism_approved {
        denials.insert(ActionGuardDenial::MechanismNotApproved);
    }
    if !context.gate.legal_review_approved {
        denials.insert(ActionGuardDenial::LegalReviewNotApproved);
    }
    if context.global_kill_switch_engaged {
        denials.insert(ActionGuardDenial::KillSwitchEngaged);
    }
    if intent.adapter.adapter_id != context.required_adapter.adapter_id
        || intent.adapter.adapter_version != context.required_adapter.adapter_version
    {
        denials.insert(ActionGuardDenial::AdapterMismatch);
    }
    if intent.adapter.client_build_profile_id != context.required_adapter.client_build_profile_id {
        denials.insert(ActionGuardDenial::ClientBuildMismatch);
    }
    if !context.allow_list.account_ids.contains(&intent.account_id) {
        denials.insert(ActionGuardDenial::AccountNotAllowed);
    }
    if !context
        .allow_list
        .conversation_ids
        .contains(&intent.conversation_id)
    {
        denials.insert(ActionGuardDenial::ConversationNotAllowed);
    }
    if !context.allow_list.capabilities.contains(&intent.capability) {
        denials.insert(ActionGuardDenial::CapabilityNotAllowed);
    }

    let expected_binding = expected_approval_binding(&context.gate.gate_decision_id, intent);
    if !is_sha256(&intent.approval.approval_id)
        || intent.approval.approver_id.is_empty()
        || !is_sha256(&intent.approval.immutable_binding_sha256)
        || intent.approval.approved_at_unix_nanoseconds
            >= intent.approval.expires_at_unix_nanoseconds
    {
        denials.insert(ActionGuardDenial::ApprovalMalformed);
    }
    if expected_binding.as_deref() != Some(&intent.approval.immutable_binding_sha256) {
        denials.insert(ActionGuardDenial::ApprovalBindingMismatch);
    }
    if context.now_unix_nanoseconds < intent.approval.approved_at_unix_nanoseconds {
        denials.insert(ActionGuardDenial::ApprovalNotYetValid);
    }
    if context.now_unix_nanoseconds >= intent.approval.expires_at_unix_nanoseconds {
        denials.insert(ActionGuardDenial::ApprovalExpired);
    }
    if context
        .consumed_approval_ids
        .contains(&intent.approval.approval_id)
    {
        denials.insert(ActionGuardDenial::ApprovalAlreadyConsumed);
    }
    if !is_sha256(&intent.idempotency_key) {
        denials.insert(ActionGuardDenial::IdempotencyKeyMalformed);
    } else if context
        .reserved_idempotency_keys
        .contains(&intent.idempotency_key)
    {
        denials.insert(ActionGuardDenial::IdempotencyKeyAlreadyReserved);
    }
    if context.rate.window_started_at_unix_nanoseconds
        >= context.rate.window_ends_at_unix_nanoseconds
        || context.rate.maximum_attempts == 0
        || context.now_unix_nanoseconds < context.rate.window_started_at_unix_nanoseconds
        || context.now_unix_nanoseconds >= context.rate.window_ends_at_unix_nanoseconds
    {
        denials.insert(ActionGuardDenial::RateLimitInvalid);
    } else if context.rate.reserved_attempts >= context.rate.maximum_attempts {
        denials.insert(ActionGuardDenial::RateLimitExceeded);
    }

    ActionGuardDecision {
        permitted: denials.is_empty(),
        denials,
        approval_binding_sha256: expected_binding,
    }
}

pub fn validate_action_lifecycle(events: &[ActionLifecycleEvent]) -> bool {
    let Some(first) = events.first() else {
        return false;
    };
    if first.state != ActionLifecycleState::Drafted
        || !valid_lifecycle_identity(first)
        || events.iter().any(|event| {
            event.action_id != first.action_id
                || event.draft_id != first.draft_id
                || event.approval_id != first.approval_id
                || event.idempotency_key != first.idempotency_key
                || !valid_lifecycle_identity(event)
        })
    {
        return false;
    }
    events.windows(2).all(|pair| {
        pair[0].observed_at_unix_nanoseconds < pair[1].observed_at_unix_nanoseconds
            && valid_lifecycle_transition(pair[0].state, pair[1].state)
    })
}

pub fn valid_lifecycle_transition(from: ActionLifecycleState, to: ActionLifecycleState) -> bool {
    matches!(
        (from, to),
        (
            ActionLifecycleState::Drafted,
            ActionLifecycleState::Approved
        ) | (
            ActionLifecycleState::Approved,
            ActionLifecycleState::Attempted
        ) | (
            ActionLifecycleState::Attempted,
            ActionLifecycleState::ObservedSent
                | ActionLifecycleState::ObservedFailed
                | ActionLifecycleState::Unknown
        ) | (
            ActionLifecycleState::Unknown,
            ActionLifecycleState::ObservedSent | ActionLifecycleState::ObservedFailed
        )
    )
}

fn valid_lifecycle_identity(event: &ActionLifecycleEvent) -> bool {
    is_sha256(&event.action_id)
        && is_sha256(&event.draft_id)
        && is_sha256(&event.approval_id)
        && is_sha256(&event.idempotency_key)
}

fn action_capability_name(capability: ActionCapability) -> &'static str {
    match capability {
        ActionCapability::TextSend => "textSend",
        ActionCapability::ReplySend => "replySend",
        ActionCapability::FileSend => "fileSend",
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn fixture() -> (ActionGuardContext, ActionAttemptIntent) {
        let adapter = ActionAdapterBinding {
            adapter_id: "disposable-visible-test-adapter".to_string(),
            adapter_version: "reviewed-v1".to_string(),
            client_build_profile_id: "wechat-macos-pinned-test-build".to_string(),
        };
        let gate = ActionGateEvidence {
            gate_decision_id: sha('a'),
            acquisition_gate_passed: true,
            restoration_gate_passed: true,
            mechanism_approved: true,
            legal_review_approved: true,
        };
        let mut intent = ActionAttemptIntent {
            capability: ActionCapability::TextSend,
            draft_id: sha('b'),
            account_id: "disposable-account".to_string(),
            conversation_id: "allow-listed-test-conversation".to_string(),
            adapter: adapter.clone(),
            idempotency_key: sha('c'),
            approval: ExternalApprovalEvidence {
                approval_id: sha('d'),
                immutable_binding_sha256: String::new(),
                approver_id: "local-owner".to_string(),
                approved_at_unix_nanoseconds: 2_000,
                expires_at_unix_nanoseconds: 4_000,
            },
        };
        intent.approval.immutable_binding_sha256 =
            expected_approval_binding(&gate.gate_decision_id, &intent).unwrap();
        let context = ActionGuardContext {
            format_version: ACTION_SAFETY_CONTRACT_VERSION,
            now_unix_nanoseconds: 3_000,
            global_kill_switch_engaged: false,
            gate,
            required_adapter: adapter,
            allow_list: ActionAllowList {
                account_ids: BTreeSet::from(["disposable-account".to_string()]),
                conversation_ids: BTreeSet::from(["allow-listed-test-conversation".to_string()]),
                capabilities: BTreeSet::from([ActionCapability::TextSend]),
            },
            rate: ActionRateState {
                window_started_at_unix_nanoseconds: 1_000,
                window_ends_at_unix_nanoseconds: 5_000,
                maximum_attempts: 1,
                reserved_attempts: 0,
            },
            consumed_approval_ids: BTreeSet::new(),
            reserved_idempotency_keys: BTreeSet::new(),
        };
        (context, intent)
    }

    #[test]
    fn exact_reviewed_action_contract_can_pass_the_pure_guard() {
        let (context, intent) = fixture();
        let decision = assess_action_attempt(&context, &intent);
        assert!(decision.permitted);
        assert!(decision.denials.is_empty());
        assert_eq!(
            decision.approval_binding_sha256.as_deref(),
            Some(intent.approval.immutable_binding_sha256.as_str())
        );
    }

    #[test]
    fn guard_fails_closed_for_every_independent_boundary() {
        let (mut context, intent) = fixture();
        context.gate.acquisition_gate_passed = false;
        context.gate.restoration_gate_passed = false;
        context.gate.mechanism_approved = false;
        context.gate.legal_review_approved = false;
        context.global_kill_switch_engaged = true;
        context.required_adapter.adapter_version = "different".to_string();
        context.required_adapter.client_build_profile_id = "drifted".to_string();
        context.allow_list.account_ids.clear();
        context.allow_list.conversation_ids.clear();
        context.allow_list.capabilities.clear();
        context
            .consumed_approval_ids
            .insert(intent.approval.approval_id.clone());
        context
            .reserved_idempotency_keys
            .insert(intent.idempotency_key.clone());
        context.rate.reserved_attempts = 1;
        let decision = assess_action_attempt(&context, &intent);
        assert!(!decision.permitted);
        for denial in [
            ActionGuardDenial::AcquisitionGateNotPassed,
            ActionGuardDenial::RestorationGateNotPassed,
            ActionGuardDenial::MechanismNotApproved,
            ActionGuardDenial::LegalReviewNotApproved,
            ActionGuardDenial::KillSwitchEngaged,
            ActionGuardDenial::AdapterMismatch,
            ActionGuardDenial::ClientBuildMismatch,
            ActionGuardDenial::AccountNotAllowed,
            ActionGuardDenial::ConversationNotAllowed,
            ActionGuardDenial::CapabilityNotAllowed,
            ActionGuardDenial::ApprovalAlreadyConsumed,
            ActionGuardDenial::IdempotencyKeyAlreadyReserved,
            ActionGuardDenial::RateLimitExceeded,
        ] {
            assert!(decision.denials.contains(&denial), "missing {denial:?}");
        }
    }

    #[test]
    fn immutable_approval_binding_changes_with_every_action_dimension() {
        let (context, intent) = fixture();
        let original = expected_approval_binding(&context.gate.gate_decision_id, &intent).unwrap();
        let mut variants = Vec::new();
        let mut value = intent.clone();
        value.capability = ActionCapability::ReplySend;
        variants.push(value);
        let mut value = intent.clone();
        value.draft_id = sha('e');
        variants.push(value);
        let mut value = intent.clone();
        value.account_id.push_str("-changed");
        variants.push(value);
        let mut value = intent.clone();
        value.conversation_id.push_str("-changed");
        variants.push(value);
        let mut value = intent.clone();
        value.adapter.adapter_id.push_str("-changed");
        variants.push(value);
        let mut value = intent.clone();
        value.adapter.adapter_version.push_str("-changed");
        variants.push(value);
        let mut value = intent.clone();
        value.adapter.client_build_profile_id.push_str("-changed");
        variants.push(value);
        for variant in variants {
            assert_ne!(
                expected_approval_binding(&context.gate.gate_decision_id, &variant).unwrap(),
                original
            );
        }
        assert_ne!(
            expected_approval_binding(&sha('f'), &intent).unwrap(),
            original
        );
    }

    #[test]
    fn approval_time_binding_and_rate_windows_fail_closed() {
        let (mut context, mut intent) = fixture();
        intent.approval.immutable_binding_sha256 = sha('f');
        intent.approval.approved_at_unix_nanoseconds = 3_001;
        intent.approval.expires_at_unix_nanoseconds = 3_001;
        context.rate.window_ends_at_unix_nanoseconds = context.now_unix_nanoseconds;
        let decision = assess_action_attempt(&context, &intent);
        assert!(!decision.permitted);
        for denial in [
            ActionGuardDenial::ApprovalMalformed,
            ActionGuardDenial::ApprovalBindingMismatch,
            ActionGuardDenial::ApprovalNotYetValid,
            ActionGuardDenial::RateLimitInvalid,
        ] {
            assert!(decision.denials.contains(&denial), "missing {denial:?}");
        }
        context.now_unix_nanoseconds = 3_001;
        let expired = assess_action_attempt(&context, &intent);
        assert!(expired
            .denials
            .contains(&ActionGuardDenial::ApprovalExpired));
    }

    #[test]
    fn lifecycle_requires_monotonic_bound_transitions_and_never_models_delivery() {
        let base = ActionLifecycleEvent {
            action_id: sha('1'),
            draft_id: sha('2'),
            approval_id: sha('3'),
            idempotency_key: sha('4'),
            state: ActionLifecycleState::Drafted,
            observed_at_unix_nanoseconds: 1,
        };
        let event = |state, observed_at| ActionLifecycleEvent {
            state,
            observed_at_unix_nanoseconds: observed_at,
            ..base.clone()
        };
        let unknown_then_sent = vec![
            base.clone(),
            event(ActionLifecycleState::Approved, 2),
            event(ActionLifecycleState::Attempted, 3),
            event(ActionLifecycleState::Unknown, 4),
            event(ActionLifecycleState::ObservedSent, 5),
        ];
        assert!(validate_action_lifecycle(&unknown_then_sent));
        assert!(!validate_action_lifecycle(&[
            base.clone(),
            event(ActionLifecycleState::ObservedSent, 2),
        ]));
        assert!(!validate_action_lifecycle(&[
            base.clone(),
            event(ActionLifecycleState::Approved, 1),
        ]));
        let mut mismatched = event(ActionLifecycleState::Approved, 2);
        mismatched.idempotency_key = sha('5');
        assert!(!validate_action_lifecycle(&[base, mismatched]));
        let serialized = serde_json::to_string(&unknown_then_sent).unwrap();
        assert!(!serialized.to_ascii_lowercase().contains("delivered"));
    }
}
