// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The reusable Put/Delete recorder backing memdb/versiondb/prefixdb batches,
//! mirroring `database/batch.go` `BatchOps` and `database/common.go` (04 §1.4).

use crate::error::Result;
use crate::traits::WriteDelete;

/// When a batch is reset, if `cap(ops) / len(ops) > MAX_EXCESS_CAPACITY_FACTOR`
/// the backing array's capacity is reduced by [`CAPACITY_REDUCTION_FACTOR`].
/// A perf-parity nicety, not a correctness requirement (`database/common.go`).
pub const MAX_EXCESS_CAPACITY_FACTOR: usize = 4;
/// The divisor applied to capacity when downsizing (`database/common.go`).
pub const CAPACITY_REDUCTION_FACTOR: usize = 2;

/// A single buffered operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchOp {
    /// The key (owned copy).
    pub key: Vec<u8>,
    /// The value (owned copy); empty for a delete.
    pub value: Vec<u8>,
    /// Whether this op is a delete.
    pub delete: bool,
}

/// A recorder of Put/Delete ops with byte-size accounting. Mirrors Go's
/// `database.BatchOps`; backends embed it and supply their own `write`.
#[derive(Default)]
pub struct BatchOps {
    /// The buffered ops, in insertion order.
    pub ops: Vec<BatchOp>,
    size: usize,
}

impl BatchOps {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a put. `key` and `value` are cloned (memory-safety contract).
    pub fn put(&mut self, key: &[u8], value: &[u8]) {
        self.ops.push(BatchOp {
            key: key.to_vec(),
            value: value.to_vec(),
            delete: false,
        });
        // size += len(key) + len(value); never overflows in practice but stays
        // checked to honor the no-silent-wrap rule.
        self.size = self
            .size
            .saturating_add(key.len())
            .saturating_add(value.len());
    }

    /// Records a delete. `key` is cloned.
    pub fn delete(&mut self, key: &[u8]) {
        self.ops.push(BatchOp {
            key: key.to_vec(),
            value: Vec::new(),
            delete: true,
        });
        self.size = self.size.saturating_add(key.len());
    }

    /// Bytes queued for writing (keys + values + deleted keys).
    pub fn size(&self) -> usize {
        self.size
    }

    /// Drops all ops, reproducing Go's capacity-shrink heuristic.
    pub fn reset(&mut self) {
        let len = self.ops.len();
        let cap = self.ops.capacity();
        // Go: if cap(b.Ops) > len(b.Ops)*MaxExcessCapacityFactor { shrink }.
        if cap > len.saturating_mul(MAX_EXCESS_CAPACITY_FACTOR) {
            self.ops = Vec::with_capacity(cap / CAPACITY_REDUCTION_FACTOR);
        } else {
            self.ops.clear();
        }
        self.size = 0;
    }

    /// Replays the buffered ops, in order, onto `w`. Propagates the first error.
    pub fn replay(&self, w: &mut dyn WriteDelete) -> Result<()> {
        for op in &self.ops {
            if op.delete {
                w.delete(&op.key)?;
            } else {
                w.put(&op.key, &op.value)?;
            }
        }
        Ok(())
    }
}

impl WriteDelete for BatchOps {
    fn put(&mut self, key: &[u8], value: &[u8]) -> Result<()> {
        BatchOps::put(self, key, value);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        BatchOps::delete(self, key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_accounting() {
        let mut b = BatchOps::new();
        b.put(b"ab", b"xyz"); // 2 + 3
        b.delete(b"k"); // + 1
        assert_eq!(b.size(), 6);
        assert_eq!(b.ops.len(), 2);
        b.reset();
        assert_eq!(b.size(), 0);
        assert!(b.ops.is_empty());
    }

    #[test]
    fn reset_shrinks_excess_capacity() {
        let mut b = BatchOps::new();
        b.ops.reserve(64);
        b.put(b"k", b"v");
        // cap (>=64) > len(1)*4 ⇒ shrink to ~cap/2.
        let cap_before = b.ops.capacity();
        b.reset();
        assert!(b.ops.capacity() < cap_before);
    }
}
