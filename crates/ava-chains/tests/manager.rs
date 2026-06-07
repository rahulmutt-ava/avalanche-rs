// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M3.26 integration tests for the chain manager primitives (specs 07 §8.1,
//! §8.3, §3.1): the `VmManager`/`Factory` registry, the bidirectional
//! `Aliaser`, and the atomic `SharedMemory` implementation.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use assert_matches::assert_matches;
use tokio_util::sync::CancellationToken;

// Crate deps the lib pulls in but this integration-test target does not name
// directly (silences `unused_crate_dependencies` for the test binary).
use ava_codec as _;
use ava_crypto as _;
use ava_snow as _;
use proptest as _;
use thiserror as _;

use ava_chains::atomic::Memory;
use ava_chains::manager::DynProbe;
use ava_chains::{Aliaser, AliaserReader, Error, Factory, ProbeableVm, VmManager};
use ava_database::{DynDatabase, MemDb};
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, Requests, SharedMemory};

/// A factory that records how many times the manager probes `Version`/`Shutdown`
/// during registration (Go `manager.RegisterFactory` calls `vm.Version()` then
/// `vm.Shutdown()` to log the version of a freshly created VM).
#[derive(Default)]
struct ProbeFactory {
    versions: Arc<AtomicU32>,
    shutdowns: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl Factory for ProbeFactory {
    async fn new_vm(&self) -> Result<Box<dyn Any + Send>, Error> {
        Ok(Box::new(DynProbe(Box::new(ProbeVm {
            versions: Arc::clone(&self.versions),
            shutdowns: Arc::clone(&self.shutdowns),
        }))))
    }
}

struct ProbeVm {
    versions: Arc<AtomicU32>,
    shutdowns: Arc<AtomicU32>,
}

#[async_trait::async_trait]
impl ProbeableVm for ProbeVm {
    async fn version(&self, _token: &CancellationToken) -> Result<String, Error> {
        self.versions.fetch_add(1, Ordering::SeqCst);
        Ok("probe/1.2.3".to_string())
    }

    async fn shutdown(&mut self, _token: &CancellationToken) -> Result<(), Error> {
        self.shutdowns.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// `register_factory_dup_errors` — a duplicate VM-ID registration errors; the
/// manager probes `Version` then `Shutdown` of the created VM on the first
/// (successful) registration.
#[tokio::test]
async fn register_factory_dup_errors() {
    let token = CancellationToken::new();
    let manager = VmManager::new();
    let vm_id = Id::from([7u8; 32]);

    let versions = Arc::new(AtomicU32::new(0));
    let shutdowns = Arc::new(AtomicU32::new(0));
    let factory = Arc::new(ProbeFactory {
        versions: Arc::clone(&versions),
        shutdowns: Arc::clone(&shutdowns),
    });

    // First registration succeeds; the manager probes Version then Shutdown.
    manager
        .register_factory(&token, vm_id, factory.clone())
        .await
        .expect("first registration");
    assert_eq!(versions.load(Ordering::SeqCst), 1, "Version probed once");
    assert_eq!(shutdowns.load(Ordering::SeqCst), 1, "Shutdown probed once");

    // The version is recorded under the VM id's primary alias.
    let versions_map = manager.versions();
    assert_eq!(
        versions_map.get(&vm_id.to_string()).map(String::as_str),
        Some("probe/1.2.3"),
        "version recorded under the primary alias"
    );

    // A second registration of the same VM id errors.
    let dup = manager.register_factory(&token, vm_id, factory).await;
    assert_matches!(dup, Err(Error::VmAlreadyRegistered));

    // `get_factory` resolves the registered factory; unknown ids are NotFound.
    assert!(manager.get_factory(vm_id).is_ok(), "registered factory");
    assert!(
        matches!(
            manager.get_factory(Id::from([0xFFu8; 32])),
            Err(Error::NotFound)
        ),
        "unknown vm id is NotFound"
    );

    let listed = manager.list_factories();
    assert_eq!(listed, vec![vm_id]);
}

/// `aliaser_primary_alias` — `primary_alias(chainID)` returns the canonical
/// (first-registered) alias, and `lookup`/`alias` round-trip.
#[test]
fn aliaser_primary_alias() {
    let aliaser = Aliaser::new();
    let chain = Id::from([3u8; 32]);

    // Before any alias, the VM id string is its own primary alias (Go default).
    aliaser.alias(chain, "C").expect("alias C");
    aliaser.alias(chain, "evm-chain").expect("alias evm-chain");

    // The primary alias is the FIRST one registered for the chain.
    assert_eq!(aliaser.primary_alias(chain).expect("primary"), "C");

    // `lookup` resolves either alias back to the chain id.
    assert_eq!(aliaser.lookup("C").expect("lookup C"), chain);
    assert_eq!(
        aliaser.lookup("evm-chain").expect("lookup evm-chain"),
        chain
    );

    // `aliases` lists every alias for the chain, in registration order.
    assert_eq!(
        aliaser.aliases(chain),
        vec!["C".to_string(), "evm-chain".to_string()]
    );

    // An unknown alias / chain is NotFound.
    assert_matches!(aliaser.lookup("unknown"), Err(Error::NotFound));
    assert_matches!(
        aliaser.primary_alias(Id::from([9u8; 32])),
        Err(Error::NotFound)
    );

    // The chain id string itself always resolves (Go `PrimaryAliasOrDefault`).
    let undefined = Id::from([42u8; 32]);
    assert_eq!(aliaser.primary_alias_or_default(undefined), undefined.to_string());
    assert_eq!(aliaser.primary_alias_or_default(chain), "C");
}

/// `shared_memory_apply_atomic` — `apply` commits a peer chain's Put/Remove
/// together with the supplied batches in one atomic write; the peer chain can
/// then `Get` the put values from its own (inbound) view.
#[test]
fn shared_memory_apply_atomic() {
    // One shared base DB backs both chains' shared-memory views.
    let base: Arc<dyn DynDatabase> = Arc::new(MemDb::new());
    let memory = Memory::new(Arc::clone(&base));

    let chain_a = Id::from([1u8; 32]);
    let chain_b = Id::from([2u8; 32]);

    let sm_a = memory.new_shared_memory(chain_a);
    let sm_b = memory.new_shared_memory(chain_b);

    // Chain A writes an element destined for chain B, atomically with a side
    // batch that writes a marker into the base DB.
    let mut side = ava_database::BatchOps::new();
    side.put(b"side-key", b"side-value");

    let mut reqs = BTreeMap::new();
    reqs.insert(
        chain_b,
        Requests {
            remove: vec![],
            put: vec![Element {
                key: b"utxo-1".to_vec(),
                value: b"payload".to_vec(),
                traits: vec![b"addr-x".to_vec()],
            }],
        },
    );
    sm_a.apply(reqs, &[side]).expect("apply");

    // The side batch landed in the base DB (committed together with the puts).
    assert_eq!(
        base.get(b"side-key").expect("side committed"),
        b"side-value"
    );

    // Chain B reads the value chain A sent it (its inbound view).
    let got = sm_b.get(chain_a, &[b"utxo-1".to_vec()]).expect("get");
    assert_eq!(got, vec![b"payload".to_vec()]);

    // An `indexed` lookup by the element's trait returns the value.
    let (values, _last_trait, _last_key) = sm_b
        .indexed(chain_a, &[b"addr-x".to_vec()], &[], &[], 10)
        .expect("indexed");
    assert_eq!(values, vec![b"payload".to_vec()]);
}
