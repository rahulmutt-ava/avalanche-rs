// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Conformance tests for the state-sync bridge: verifies that
//! `convert_state_sync()` correctly bridges a `SyncableVm<SP>` into
//! `ava_vm::StateSyncableVm`, and that the wrapped summary's `accept` forwards
//! to the VM.

use std::collections::HashMap;
use std::sync::Arc;

use assert_matches::assert_matches;
use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use ava_saevm_adaptor::{SummaryProperties, SyncableVm, convert_state_sync};
use ava_types::id::Id;
use ava_vm::block::{StateSyncMode, StateSyncableVm as ConsensusStateSyncableVm};
use ava_vm::{Error as VmError, Result as VmResult};

// ---------------------------------------------------------------------------
// FakeSummary — a trivial SummaryProperties newtype
// ---------------------------------------------------------------------------

/// Minimal summary-properties value for tests.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FakeSummary {
    id: Id,
    height: u64,
    bytes: Vec<u8>,
}

impl SummaryProperties for FakeSummary {
    fn id(&self) -> Id {
        self.id
    }

    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn height(&self) -> u64 {
        self.height
    }
}

// ---------------------------------------------------------------------------
// MockSyncableVm — in-memory map of height -> summary
// ---------------------------------------------------------------------------

/// A local mock implementing the SAE-friendly `SyncableVm<FakeSummary>`.
struct MockSyncableVm {
    enabled: bool,
    /// The mode `accept_summary` reports back.
    accept_mode: StateSyncMode,
    /// height -> summary
    by_height: HashMap<u64, FakeSummary>,
    /// the "last" summary (most recent), if any.
    last: Option<FakeSummary>,
    /// the ongoing-sync summary, if any.
    ongoing: Option<FakeSummary>,
}

impl MockSyncableVm {
    fn new() -> Self {
        Self {
            enabled: true,
            accept_mode: StateSyncMode::Static,
            by_height: HashMap::new(),
            last: None,
            ongoing: None,
        }
    }

    fn with_summary(mut self, s: FakeSummary) -> Self {
        self.by_height.insert(s.height(), s.clone());
        self.last = Some(s);
        self
    }
}

#[async_trait]
impl SyncableVm<FakeSummary> for MockSyncableVm {
    async fn state_sync_enabled(&self, _token: &CancellationToken) -> VmResult<bool> {
        Ok(self.enabled)
    }

    async fn get_ongoing_sync_state_summary(
        &self,
        _token: &CancellationToken,
    ) -> VmResult<FakeSummary> {
        self.ongoing.clone().ok_or(VmError::NotFound)
    }

    async fn get_last_state_summary(&self, _token: &CancellationToken) -> VmResult<FakeSummary> {
        self.last.clone().ok_or(VmError::NotFound)
    }

    async fn parse_state_summary(
        &self,
        _token: &CancellationToken,
        bytes: &[u8],
    ) -> VmResult<FakeSummary> {
        // Round-trip: the first byte is the height, the rest are the payload;
        // the id byte mirrors the height (height + 1) for a deterministic id.
        let height = u64::from(bytes.first().copied().unwrap_or(0));
        Ok(FakeSummary {
            id: Id::from([height_to_byte(height); 32]),
            height,
            bytes: bytes.to_vec(),
        })
    }

    async fn get_state_summary(
        &self,
        _token: &CancellationToken,
        height: u64,
    ) -> VmResult<FakeSummary> {
        self.by_height
            .get(&height)
            .cloned()
            .ok_or(VmError::NotFound)
    }

    async fn accept_summary(
        &self,
        _token: &CancellationToken,
        _summary: &FakeSummary,
    ) -> VmResult<StateSyncMode> {
        Ok(self.accept_mode)
    }
}

fn height_to_byte(height: u64) -> u8 {
    // Tests only use small heights; the low byte is sufficient and deterministic.
    u8::try_from(height & 0xff).unwrap_or(0).wrapping_add(1)
}

fn mk_summary(height: u64) -> FakeSummary {
    FakeSummary {
        id: Id::from([height_to_byte(height); 32]),
        height,
        bytes: vec![u8::try_from(height & 0xff).unwrap_or(0), 0xaa, 0xbb],
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_last_state_summary_matches_stored() {
    let summary = mk_summary(7);
    let vm = Arc::new(Mutex::new(
        MockSyncableVm::new().with_summary(summary.clone()),
    ));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let got = adaptor
        .get_last_state_summary(&token)
        .await
        .expect("get_last_state_summary");

    assert_eq!(got.id(), summary.id(), "id matches stored");
    assert_eq!(got.height(), summary.height(), "height matches stored");
    assert_eq!(got.bytes(), summary.bytes(), "bytes match stored");
}

#[tokio::test]
async fn get_state_summary_by_height_matches_stored() {
    let summary = mk_summary(42);
    let vm = Arc::new(Mutex::new(
        MockSyncableVm::new().with_summary(summary.clone()),
    ));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let got = adaptor
        .get_state_summary(&token, 42)
        .await
        .expect("get_state_summary");

    assert_eq!(got.id(), summary.id(), "id matches stored");
    assert_eq!(got.height(), 42, "height matches request");
    assert_eq!(got.bytes(), summary.bytes(), "bytes match stored");
}

#[tokio::test]
async fn parse_state_summary_round_trips_bytes() {
    let vm = Arc::new(Mutex::new(MockSyncableVm::new()));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let raw = vec![5u8, 1, 2, 3];
    let got = adaptor
        .parse_state_summary(&token, &raw)
        .await
        .expect("parse_state_summary");

    assert_eq!(got.bytes(), raw.as_slice(), "bytes round-trip");
    assert_eq!(got.height(), 5, "height decoded from first byte");
}

#[tokio::test]
async fn wrapped_summary_accept_forwards_to_vm() {
    let summary = mk_summary(3);
    let mut mock = MockSyncableVm::new().with_summary(summary);
    mock.accept_mode = StateSyncMode::Static;
    let vm = Arc::new(Mutex::new(mock));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let got = adaptor
        .get_last_state_summary(&token)
        .await
        .expect("get_last_state_summary");

    let mode = got.accept(&token).await.expect("accept");
    assert_eq!(mode, StateSyncMode::Static, "accept returns the VM's mode");
}

#[tokio::test]
async fn wrapped_summary_accept_forwards_dynamic_mode() {
    let summary = mk_summary(9);
    let mut mock = MockSyncableVm::new().with_summary(summary);
    mock.accept_mode = StateSyncMode::Dynamic;
    let vm = Arc::new(Mutex::new(mock));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let got = adaptor
        .get_last_state_summary(&token)
        .await
        .expect("get_last_state_summary");

    let mode = got.accept(&token).await.expect("accept");
    assert_eq!(mode, StateSyncMode::Dynamic, "accept returns Dynamic mode");
}

#[tokio::test]
async fn state_sync_enabled_reflects_flag() {
    let vm = Arc::new(Mutex::new(MockSyncableVm::new()));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    let enabled = adaptor
        .state_sync_enabled(&token)
        .await
        .expect("state_sync_enabled");
    assert!(enabled, "enabled by default");

    {
        let mut guard = vm.lock().await;
        guard.enabled = false;
    }

    let enabled = adaptor
        .state_sync_enabled(&token)
        .await
        .expect("state_sync_enabled after toggle");
    assert!(!enabled, "reflects disabled flag");
}

#[tokio::test]
async fn not_found_error_propagates() {
    // Empty VM: no summary stored anywhere.
    let vm = Arc::new(Mutex::new(MockSyncableVm::new()));
    let adaptor = convert_state_sync(Arc::clone(&vm));
    let token = CancellationToken::new();

    // `Arc<dyn StateSummary>` is not `Debug`, so collapse the `Ok` payload to
    // `()` before matching the error variant with `assert_matches!`.
    let last_err = adaptor.get_last_state_summary(&token).await.map(|_| ());
    assert_matches!(last_err, Err(VmError::NotFound), "missing last summary");

    let height_err = adaptor.get_state_summary(&token, 99).await.map(|_| ());
    assert_matches!(
        height_err,
        Err(VmError::NotFound),
        "missing summary at height"
    );

    let ongoing_err = adaptor
        .get_ongoing_sync_state_summary(&token)
        .await
        .map(|_| ());
    assert_matches!(
        ongoing_err,
        Err(VmError::NotFound),
        "no ongoing sync summary"
    );
}
