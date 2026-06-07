// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The outstanding-poll set with early termination (port of
//! `snow/consensus/snowman/poll/`, specs 06 §4.2).
//!
//! A [`PollSet`] tracks the polls the Snowman engine has issued but not yet
//! resolved. Each [`Poll`] records the validators it is waiting on and the votes
//! received so far. A poll terminates early — *before* every sampled validator
//! responds — as soon as the outstanding responses can no longer change the
//! `alpha_preference`/`alpha_confidence` outcome (the [`EarlyTermFactory`]).
//!
//! ## Port note (incremental early-term)
//!
//! Go's `early_term_traversal.go` builds a full transitive vote graph (with
//! shared-prefix bifurcations) on every `Finished()` call. We instead require
//! the engine to bubble each chit to the nearest *processing ancestor* before
//! [`PollSet::vote`] (so the votes bag already holds ancestor IDs), then apply
//! the per-ID termination predicate (cases 1–4 of Go's `shouldTerminate`)
//! incrementally as votes arrive. This evaluates the predicate in O(1) per chit
//! rather than rescanning, and only changes *when* a poll completes — never the
//! resulting decision (the safety argument of specs 06 §11). The shared-prefix
//! short-circuit (which can only ever finish a poll *earlier*) is deferred; see
//! `tests/PORTING.md`.

use std::collections::BTreeMap;

use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_utils::bag::Bag;

/// Decides when a poll has gathered enough information to terminate. Produced by
/// an [`EarlyTermFactory`].
pub trait PollFactory: Send {
    /// Builds a new poll waiting on the supplied weighted validator set.
    fn new_poll(&self, validators: Bag<NodeId>) -> Poll;
}

/// `poll.NewEarlyTermFactory` — produces polls that terminate as soon as the
/// alpha outcome is locked in.
#[derive(Clone, Copy, Debug)]
pub struct EarlyTermFactory {
    alpha_preference: u32,
    alpha_confidence: u32,
}

impl EarlyTermFactory {
    /// Builds a factory parameterized by the alpha thresholds.
    #[must_use]
    pub fn new(alpha_preference: u32, alpha_confidence: u32) -> Self {
        Self {
            alpha_preference,
            alpha_confidence,
        }
    }
}

impl PollFactory for EarlyTermFactory {
    fn new_poll(&self, validators: Bag<NodeId>) -> Poll {
        Poll {
            polled: validators,
            votes: Bag::new(),
            alpha_preference: self.alpha_preference,
            alpha_confidence: self.alpha_confidence,
            finished: false,
        }
    }
}

/// An outstanding poll (port of `poll.earlyTermPoll`).
#[derive(Debug)]
pub struct Poll {
    /// The validators still expected to respond, weighted by stake.
    polled: Bag<NodeId>,
    /// The votes received so far, weighted by the responding validator's stake.
    votes: Bag<Id>,
    alpha_preference: u32,
    alpha_confidence: u32,
    finished: bool,
}

impl Poll {
    /// Registers `vote` (an already-bubbled processing-ancestor id) from `vdr`.
    /// A validator that has already responded (or was never polled) is ignored.
    pub fn vote(&mut self, vdr: NodeId, vote: Id) {
        let count = self.polled.count(&vdr);
        if count == 0 {
            return;
        }
        self.remove_polled(vdr, count);
        self.votes.add_count(vote, count);
    }

    /// Drops a future response from `vdr` (a query failure / timeout).
    pub fn drop(&mut self, vdr: NodeId) {
        let count = self.polled.count(&vdr);
        if count == 0 {
            return;
        }
        self.remove_polled(vdr, count);
    }

    /// Removes `vdr`'s `count` weight from the outstanding set. `Bag` has no
    /// `remove`, so the remaining weight is rebuilt without `vdr`.
    fn remove_polled(&mut self, vdr: NodeId, count: usize) {
        let _ = count;
        let mut remaining = Bag::new();
        for node in self.polled.list() {
            if node == vdr {
                continue;
            }
            remaining.add_count(node, self.polled.count(&node));
        }
        self.polled = remaining;
    }

    /// Whether the poll has terminated (cases 1–4 of Go `earlyTermPoll`).
    #[must_use]
    pub fn finished(&mut self) -> bool {
        if self.finished {
            return true;
        }

        let remaining = self.polled.len();
        // Case 1: no outstanding votes.
        if remaining == 0 {
            self.finished = true;
            return true;
        }

        let received = self.votes.len();
        let max_possible = received.saturating_add(remaining);
        let alpha_pref = self.alpha_preference as usize;
        let alpha_conf = self.alpha_confidence as usize;

        // Case 2: alpha_preference can never be reached by any single id.
        if max_possible < alpha_pref {
            self.finished = true;
            return true;
        }

        // Cases 2–4 per id: terminate only if *every* id's snowflake instance
        // can no longer be improved by further voting.
        let ids = self.votes.list();
        if ids.is_empty() {
            return false;
        }
        let mut all_settled = true;
        for id in &ids {
            let freq = self.votes.count(id);
            if !Self::should_terminate(freq, remaining, alpha_pref, alpha_conf) {
                all_settled = false;
                break;
            }
        }
        if all_settled {
            self.finished = true;
        }
        self.finished
    }

    /// Per-id termination predicate (Go `shouldTerminate`).
    fn should_terminate(
        freq: usize,
        remaining: usize,
        alpha_pref: usize,
        alpha_conf: usize,
    ) -> bool {
        let max_possible = freq.saturating_add(remaining);
        max_possible < alpha_pref // Case 2
            || (freq >= alpha_pref && max_possible < alpha_conf) // Case 3
            || freq >= alpha_conf // Case 4
    }

    /// The accumulated votes (the poll result). Cloned so the poll can be
    /// inspected without consuming it.
    #[must_use]
    pub fn result(&self) -> Bag<Id> {
        self.votes.clone()
    }
}

/// The set of outstanding polls keyed by request id (port of `poll.set`).
///
/// Polls complete in request-id order: a finished poll is only drained once all
/// older polls have also finished, matching Go's `processFinishedPolls` (so
/// `record_poll` is always applied in poll order — a determinism requirement).
pub struct PollSet<F: PollFactory> {
    factory: F,
    polls: BTreeMap<u32, Poll>,
}

impl<F: PollFactory> PollSet<F> {
    /// Builds an empty poll set with the supplied factory.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            polls: BTreeMap::new(),
        }
    }

    /// Registers a new poll for `request_id` over the weighted `validators` set.
    /// Returns `false` (and registers nothing) if `request_id` is already in
    /// flight, mirroring Go's duplicate-request drop.
    pub fn add(&mut self, request_id: u32, validators: Bag<NodeId>) -> bool {
        if self.polls.contains_key(&request_id) {
            return false;
        }
        self.polls.insert(request_id, self.factory.new_poll(validators));
        true
    }

    /// Registers `vote` from `vdr` for `request_id`. Returns the result bags of
    /// every poll that completed as a consequence (oldest first); empty if none.
    pub fn vote(&mut self, request_id: u32, vdr: NodeId, vote: Id) -> Vec<Bag<Id>> {
        let Some(poll) = self.polls.get_mut(&request_id) else {
            return Vec::new();
        };
        poll.vote(vdr, vote);
        if !poll.finished() {
            return Vec::new();
        }
        self.drain_finished()
    }

    /// Drops `vdr`'s response for `request_id` (timeout / query failure).
    /// Returns the result bags of every poll that completed (oldest first).
    pub fn drop(&mut self, request_id: u32, vdr: NodeId) -> Vec<Bag<Id>> {
        let Some(poll) = self.polls.get_mut(&request_id) else {
            return Vec::new();
        };
        poll.drop(vdr);
        if !poll.finished() {
            return Vec::new();
        }
        self.drain_finished()
    }

    /// Drains finished polls from oldest to newest, stopping at the first
    /// unfinished poll (Go `processFinishedPolls`).
    fn drain_finished(&mut self) -> Vec<Bag<Id>> {
        let mut results = Vec::new();
        let keys: Vec<u32> = self.polls.keys().copied().collect();
        for key in keys {
            let finished = self.polls.get_mut(&key).is_some_and(Poll::finished);
            if !finished {
                break;
            }
            if let Some(poll) = self.polls.remove(&key) {
                results.push(poll.result());
            }
        }
        results
    }

    /// The number of outstanding polls.
    #[must_use]
    pub fn len(&self) -> usize {
        self.polls.len()
    }

    /// Whether there are no outstanding polls.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.polls.is_empty()
    }

    /// Whether `request_id` is an outstanding poll.
    #[must_use]
    pub fn contains(&self, request_id: u32) -> bool {
        self.polls.contains_key(&request_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(b: u8) -> NodeId {
        NodeId::from([b; 20])
    }

    fn id(b: u8) -> Id {
        Id::from([b; 32])
    }

    fn validators(weights: &[(NodeId, usize)]) -> Bag<NodeId> {
        let mut bag = Bag::new();
        for (n, w) in weights {
            bag.add_count(*n, *w);
        }
        bag
    }

    /// A poll terminates on case 4 (an id reached alpha_confidence) before the
    /// final validator responds.
    #[test]
    fn early_term_case4() {
        let factory = EarlyTermFactory::new(2, 2);
        let mut set = PollSet::new(factory);
        let vdrs = validators(&[(node(1), 1), (node(2), 1), (node(3), 1)]);
        assert!(set.add(7, vdrs));

        // First vote: 1 for id(9), not enough.
        assert!(set.vote(7, node(1), id(9)).is_empty());
        // Second vote: 2 for id(9) == alpha_confidence -> terminates even though
        // node(3) has not responded.
        let results = set.vote(7, node(2), id(9));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].count(&id(9)), 2);
        assert!(set.is_empty());
    }

    /// A poll terminates on case 2 (alpha_preference unreachable for every id)
    /// before the final validator responds: two conflicting votes split the
    /// stake so neither id can reach alpha=3 with a single outstanding vote.
    #[test]
    fn early_term_case2_unreachable() {
        let factory = EarlyTermFactory::new(3, 3);
        let mut set = PollSet::new(factory);
        let vdrs = validators(&[(node(1), 1), (node(2), 1), (node(3), 1)]);
        assert!(set.add(1, vdrs));

        // First conflicting vote: id(9)=1, two validators still outstanding, so
        // id(9) could still reach 3 (1+2). Not finished.
        assert!(set.vote(1, node(1), id(9)).is_empty());
        // Second conflicting vote: id(9)=1, id(8)=1, one validator outstanding.
        // Neither id can reach alpha=3 (max 2 each) -> early-term case 2 fires.
        let results = set.vote(1, node(2), id(8));
        assert_eq!(results.len(), 1);
        assert!(set.is_empty());
    }

    /// A poll terminates via `drop` when alpha becomes globally unreachable
    /// (case 2 on `votes.len() + remaining`). With k=4 and alpha=3, the first
    /// drop leaves max possible = 3 (still reachable); the second drop makes it
    /// 2 < 3 so the poll finishes.
    #[test]
    fn early_term_drop_global_unreachable() {
        let factory = EarlyTermFactory::new(3, 3);
        let mut set = PollSet::new(factory);
        let vdrs = validators(&[(node(1), 1), (node(2), 1), (node(3), 1), (node(4), 1)]);
        assert!(set.add(1, vdrs));

        // No votes received; first drop -> 3 outstanding, max 3 == alpha (open).
        assert!(set.drop(1, node(1)).is_empty());
        // Second drop -> 2 outstanding, max 2 < alpha=3: case 2 fires.
        let results = set.drop(1, node(2));
        assert_eq!(results.len(), 1);
    }

    /// Polls drain in request-id order: an older unfinished poll blocks a newer
    /// finished one.
    #[test]
    fn polls_drain_in_order() {
        let factory = EarlyTermFactory::new(1, 1);
        let mut set = PollSet::new(factory);
        set.add(1, validators(&[(node(1), 1), (node(2), 1)]));
        set.add(2, validators(&[(node(1), 1)]));

        // Poll 2 finishes (alpha=1) but poll 1 is still open -> nothing drains.
        let results = set.vote(2, node(1), id(5));
        assert!(results.is_empty());
        assert_eq!(set.len(), 2);

        // Poll 1 finishes -> both drain, oldest first.
        let results = set.vote(1, node(1), id(5));
        assert_eq!(results.len(), 2);
        assert!(set.is_empty());
    }
}
