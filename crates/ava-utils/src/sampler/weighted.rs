// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Weighted sampler — cumulative-weight heap (`weighted_heap.go`).
//!
//! Ported verbatim from `specs/03-core-primitives.md` §4.1 and Go
//! `utils/sampler/weighted_heap.go`. Builds a heap of
//! `{weight, cumulative_weight, index}`, stable-sorts by `(weight desc,
//! index asc)`, accumulates cumulative weights leaf→root with **checked add**
//! (`parent = (i-1) >> 1`), and walks the heap on `sample(value)` exactly as Go.
//! Owning spec: `specs/03-core-primitives.md` §4.1.

use crate::error::Result;
use crate::math;

/// A weighted sampler mapping a `value` in `[0, total_weight)` to an index (Go
/// `sampler.Weighted`).
pub trait Weighted {
    /// Initializes the sampler with per-index weights. Errors on weight-sum
    /// overflow (Go `Initialize`).
    ///
    /// # Errors
    /// Returns [`crate::error::Error::Overflow`] if the weights sum overflows `u64`.
    fn initialize(&mut self, weights: &[u64]) -> Result<()>;
    /// Maps `value` to the index whose cumulative-weight interval contains it,
    /// or `None` if out of range (Go `Sample`).
    fn sample(&mut self, value: u64) -> Option<usize>;
}

#[derive(Clone, Copy)]
struct Element {
    weight: u64,
    cumulative_weight: u64,
    index: usize,
}

/// Heap-based deterministic weighted sampler (Go `weightedHeap`).
#[derive(Default)]
pub struct WeightedHeap {
    heap: Vec<Element>,
}

impl WeightedHeap {
    /// An empty weighted heap.
    #[must_use]
    pub fn new() -> Self {
        Self { heap: Vec::new() }
    }
}

impl Weighted for WeightedHeap {
    fn initialize(&mut self, weights: &[u64]) -> Result<()> {
        let mut heap: Vec<Element> = weights
            .iter()
            .enumerate()
            .map(|(index, &weight)| Element {
                weight,
                cumulative_weight: weight,
                index,
            })
            .collect();

        // Stable-sort by (weight desc, original index asc) — Go
        // `weightedHeapElement.Less`.
        heap.sort_by(|a, b| {
            b.weight
                .cmp(&a.weight)
                .then_with(|| a.index.cmp(&b.index))
        });

        // Accumulate cumulative weights from leaves to root with checked add.
        let mut i = heap.len();
        while i > 1 {
            i -= 1;
            let parent = (i - 1) >> 1;
            heap[parent].cumulative_weight =
                math::add(heap[parent].cumulative_weight, heap[i].cumulative_weight)?;
        }

        self.heap = heap;
        Ok(())
    }

    fn sample(&mut self, mut value: u64) -> Option<usize> {
        if self.heap.is_empty() || value >= self.heap[0].cumulative_weight {
            return None;
        }
        let mut index = 0;
        loop {
            let current = self.heap[index];
            if value < current.weight {
                return Some(current.index);
            }
            value -= current.weight;

            // Move to the left child.
            index = index * 2 + 1;
            let left_weight = self.heap[index].cumulative_weight;
            if left_weight <= value {
                // Skip the left subtree; go to the right child.
                value -= left_weight;
                index += 1;
            }
        }
    }
}
