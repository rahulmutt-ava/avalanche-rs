// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the `fx` framework (`ava-vm::fx`, specs 07 §4.1) and
//! the `ava-secp256k1fx` `FxInstance` registration (M3.20).
//!
//! Covers the two contracts the spec fixes:
//!
//! * the `&dyn Any` boundary — a mismatched concrete type passed to
//!   `verify_transfer`/`verify_permission`/`verify_operation`/`create_output`
//!   maps to the exact Go wrong-type sentinel via `downcast_ref`, and
//! * `initialize` registers secp256k1fx's five codec typeIDs into the host VM's
//!   codec registry.

#![allow(
    unused_crate_dependencies,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing
)]

use std::any::Any;
use std::sync::{Arc, Mutex};

use assert_matches::assert_matches;

use ava_secp256k1fx::instance::Secp256k1Fx;
use ava_utils::clock::RealClock;
use ava_vm::error::Error;
use ava_vm::fx::{CodecRegistry, FxInstance, FxVm, UnsignedTx};

/// A minimal `FxVm` recording the typeIDs an fx registers at `initialize`.
#[derive(Default)]
struct TestFxVm {
    registry: TestRegistry,
}

#[derive(Default)]
struct TestRegistry {
    registered: Mutex<Vec<String>>,
    next: Mutex<u32>,
}

impl CodecRegistry for TestRegistry {
    fn register_type(&self, name: &str) -> ava_vm::error::Result<u32> {
        let mut next = self.next.lock().expect("lock");
        let id = *next;
        *next = next.wrapping_add(1);
        self.registered.lock().expect("lock").push(name.to_string());
        Ok(id)
    }
}

impl FxVm for TestFxVm {
    fn codec_registry(&self) -> &dyn CodecRegistry {
        &self.registry
    }

    fn clock(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }
}

/// A concrete `UnsignedTx` so we can pass a *valid* tx but mismatched components.
struct TestTx(Vec<u8>);
impl UnsignedTx for TestTx {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A type that matches NONE of the fx's expected concrete types.
struct Bogus;

fn new_fx() -> Secp256k1Fx {
    Secp256k1Fx::new(Arc::new(RealClock))
}

#[test]
fn fx_wrong_type_downcast() {
    let fx = new_fx();
    let tx = TestTx(vec![1, 2, 3]);
    let bogus: &dyn Any = &Bogus;

    // verify_transfer: a bogus input ⇒ WrongInputType.
    assert_matches!(
        fx.verify_transfer(&tx, bogus, bogus, bogus),
        Err(Error::WrongInputType)
    );
    // verify_permission: bogus input ⇒ WrongInputType.
    assert_matches!(
        fx.verify_permission(&tx, bogus, bogus, bogus),
        Err(Error::WrongInputType)
    );
    // verify_operation: bogus op ⇒ WrongOpType.
    assert_matches!(
        fx.verify_operation(&tx, bogus, bogus, &[bogus]),
        Err(Error::WrongOpType)
    );
    // create_output: bogus owner ⇒ WrongOwnerType. (`Arc<dyn State>` is not
    // `Debug`, so map the `Ok` away before matching the error.)
    assert_matches!(
        fx.create_output(7, bogus).map(|_| ()),
        Err(Error::WrongOwnerType)
    );
}

#[test]
fn secp256k1fx_registers_codec_types() {
    let mut fx = new_fx();
    let vm = Arc::new(TestFxVm::default());

    fx.initialize(vm.clone()).expect("initialize");

    let registered = vm.registry.registered.lock().expect("lock").clone();
    assert_eq!(
        registered,
        vec![
            "TransferInput".to_string(),
            "MintOutput".to_string(),
            "TransferOutput".to_string(),
            "MintOperation".to_string(),
            "Credential".to_string(),
        ]
    );
}
