// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The snowball [`Tree`]: a modified Patricia/radix tree over 256-bit choice
//! ids (specs 06 §2.3; Go `snow/consensus/snowball/tree.go`).
//!
//! The tree amortizes a single multi-choice [`Consensus`] instance over a run
//! of unary snow instances (shared bit prefixes) that split into binary
//! instances at the first differing bit. Votes (a [`Bag<Id>`]) are routed down
//! the matching bit-prefix path to the snowflake/snowball nodes.
//!
//! This is a transition-exact port of the Go tree, including its five `Add`
//! split cases and the `should_reset` falter optimization. The [`fmt::Display`]
//! impl reproduces Go `Tree.String()` byte-for-byte so the golden vectors can
//! assert tree structure directly.

use std::fmt;

use ava_types::bits::{NUM_BITS, equal_subset, first_difference_subset};
use ava_types::id::Id;
use ava_utils::bag::Bag;

use super::Parameters;
use super::consensus::{BinaryInstance, Consensus, Factory, UnaryInstance};

/// A modified Patricia tree implementing [`Consensus`] over 256-bit choices.
pub struct Tree<F: Factory> {
    /// The root node (the first snow instance), holding the whole sub-tree.
    node: Node<F>,
    /// The snowball configuration applied to every instance.
    params: Parameters,
    /// Falter optimization: when a poll fails to reach an alpha majority, the
    /// whole sub-tree must reset on the next traversal. Rather than walking the
    /// full tree, this flag defers the reset to the next `record_poll`.
    should_reset: bool,
    /// Produces new snow instances as the tree splits.
    factory: F,
}

impl<F: Factory> Tree<F> {
    /// Builds a tree initially preferring `choice` (Go `NewTree`).
    #[must_use]
    pub fn new(factory: F, params: Parameters, choice: Id) -> Self {
        let snow = factory.new_unary(params);
        let node = Node::Unary(Box::new(UnaryNode {
            preference: choice,
            decided_prefix: 0,
            common_prefix: NUM_BITS, // The initial state has no conflicts.
            snow,
            should_reset: false,
            child: None,
        }));
        Self {
            node,
            params,
            should_reset: false,
            factory,
        }
    }
}

impl<F: Factory> Consensus for Tree<F> {
    fn add(&mut self, choice: Id) {
        let prefix = self.node.decided_prefix();
        // Make sure we haven't already decided against this new id.
        if equal_subset(0, prefix, &self.node.preference(), &choice) {
            let node = std::mem::replace(&mut self.node, Node::placeholder());
            self.node = node.add(&self.factory, &self.params, choice);
        }
    }

    fn preference(&self) -> Id {
        self.node.preference()
    }

    fn record_poll(&mut self, votes: &Bag<Id>) -> bool {
        // Restrict to votes whose decided-prefix bits match the preference; any
        // others are for rejected operations.
        let decided_prefix = self.node.decided_prefix();
        let preference = self.preference();
        let filtered = filter_bag(votes, |id| equal_subset(0, decided_prefix, &preference, id));

        let node = std::mem::replace(&mut self.node, Node::placeholder());
        let (new_node, successful) =
            node.record_poll(&self.factory, &self.params, &filtered, self.should_reset);
        self.node = new_node;

        // The reset has been passed into the instance.
        self.should_reset = false;
        successful
    }

    fn record_unsuccessful_poll(&mut self) {
        self.should_reset = true;
    }

    fn finalized(&self) -> bool {
        self.node.finalized()
    }
}

impl<F: Factory> fmt::Display for Tree<F> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Pre-order traversal with a "    " indent per depth, mirroring Go
        // `Tree.String()` (which uses an explicit stack; same output).
        let mut out = String::new();
        self.node.write_printable(&mut out, "");
        // Strip the trailing newline.
        write!(f, "{}", out.trim_end_matches('\n'))
    }
}

/// A node in the patricia tree: either a unary run or a single binary split.
///
/// `Empty` is a transient placeholder used only while an owned node is being
/// moved out for a recursive rebuild (it is always overwritten before any other
/// method is called on it); it never appears in a live tree.
enum Node<F: Factory> {
    /// A run of identical unary instances over `[decided_prefix, common_prefix)`.
    Unary(Box<UnaryNode<F>>),
    /// A single binary instance deciding on one bit.
    Binary(Box<BinaryNode<F>>),
    /// A transient hole (see the type-level note).
    Empty,
}

impl<F: Factory> Node<F> {
    /// A throwaway node used while taking ownership during recursive rebuilds.
    /// Never observed (immediately overwritten by the returned node).
    fn placeholder() -> Self {
        Node::Empty
    }

    fn preference(&self) -> Id {
        match self {
            Node::Unary(u) => u.preference,
            Node::Binary(b) => b.preferences[b.snow.preference() as usize],
            Node::Empty => Id::EMPTY,
        }
    }

    fn decided_prefix(&self) -> i32 {
        match self {
            Node::Unary(u) => u.decided_prefix,
            Node::Binary(b) => b.bit,
            Node::Empty => NUM_BITS,
        }
    }

    fn finalized(&self) -> bool {
        match self {
            Node::Unary(u) => u.snow.finalized(),
            Node::Binary(b) => b.snow.finalized(),
            Node::Empty => false,
        }
    }

    fn add(self, factory: &F, params: &Parameters, new_choice: Id) -> Self {
        match self {
            Node::Unary(u) => u.add(factory, params, new_choice),
            Node::Binary(b) => b.add(factory, params, new_choice),
            Node::Empty => Node::Empty,
        }
    }

    fn record_poll(
        self,
        factory: &F,
        params: &Parameters,
        votes: &Bag<Id>,
        reset: bool,
    ) -> (Self, bool) {
        match self {
            Node::Unary(u) => u.record_poll(factory, params, votes, reset),
            Node::Binary(b) => b.record_poll(factory, params, votes, reset),
            Node::Empty => (Node::Empty, false),
        }
    }

    fn write_printable(&self, out: &mut String, prefix: &str) {
        match self {
            Node::Unary(u) => {
                out.push_str(prefix);
                out.push_str(&format!(
                    "{} Bits = [{}, {})",
                    u.snow, u.decided_prefix, u.common_prefix
                ));
                out.push('\n');
                if let Some(child) = &u.child {
                    let child_prefix = format!("{prefix}    ");
                    child.write_printable(out, &child_prefix);
                }
            }
            Node::Binary(b) => {
                out.push_str(prefix);
                out.push_str(&format!("{} Bit = {}", b.snow, b.bit));
                out.push('\n');
                if let (Some(c0), Some(c1)) = (&b.children[0], &b.children[1]) {
                    let child_prefix = format!("{prefix}    ");
                    // Go's binaryNode.Printable returns [children[1],
                    // children[0]]; the String() stack pops from the end, so
                    // children[0] is printed before children[1].
                    c0.write_printable(out, &child_prefix);
                    c1.write_printable(out, &child_prefix);
                }
            }
            Node::Empty => {}
        }
    }
}

/// A node handling a run of identical unary snow instances.
struct UnaryNode<F: Factory> {
    /// The preferred choice at every branch in this sub-tree.
    preference: Id,
    /// The last bit in the prefix assumed decided (range `[0, 255)`).
    decided_prefix: i32,
    /// The last bit this node transitively references (range
    /// `(decided_prefix, 256]`).
    common_prefix: i32,
    /// The unary decision instance.
    snow: F::Unary,
    /// Falter continuation from the tree (specs 06 §2.3).
    should_reset: bool,
    /// The (possibly absent) node voting on the next bits.
    child: Option<Node<F>>,
}

impl<F: Factory> UnaryNode<F> {
    #[allow(clippy::too_many_lines)]
    fn add(mut self: Box<Self>, factory: &F, params: &Parameters, new_choice: Id) -> Node<F> {
        if self.snow.finalized() {
            // Only happens if the tree is finalized, or it's a leaf node.
            return Node::Unary(self);
        }

        let Some(index) = first_difference_subset(
            self.decided_prefix,
            self.common_prefix,
            &self.preference,
            &new_choice,
        ) else {
            // No first difference: this node shouldn't be split.
            if let Some(child) = self.child.take() {
                // This node finalizes before any child, so new_choice matches
                // the child's prefix.
                self.child = Some(child.add(factory, params, new_choice));
            }
            // If child is None, this is re-adding the same choice: a no-op.
            return Node::Unary(self);
        };
        let index = index as i32;

        // The difference was found: this node must be split.
        let bit = self.preference.bit(index as usize); // The currently preferred bit.
        let mut b = Box::new(BinaryNode {
            preferences: [Id::EMPTY; 2],
            bit: index,
            snow: self.snow.extend(bit),
            should_reset: [self.should_reset, self.should_reset],
            children: [None, None],
        });
        b.preferences[bit as usize] = self.preference;
        b.preferences[1 - bit as usize] = new_choice;

        let new_child_snow = factory.new_unary(*params);
        let new_child = Node::Unary(Box::new(UnaryNode {
            preference: new_choice,
            decided_prefix: index + 1, // This branch is decided in its favor.
            common_prefix: NUM_BITS,   // No conflicts under this branch.
            snow: new_child_snow,
            should_reset: false,
            child: None,
        }));

        if self.decided_prefix == self.common_prefix - 1 {
            // Case 2: only voting over one bit.
            let had_child = self.child.is_some();
            b.children[bit as usize] = self.child.take();
            if had_child {
                b.children[1 - bit as usize] = Some(new_child);
            }
            Node::Binary(b)
        } else if index == self.decided_prefix {
            // Case 3: split on the first bit.
            self.decided_prefix += 1;
            b.children[1 - bit as usize] = Some(new_child);
            b.children[bit as usize] = Some(Node::Unary(self));
            Node::Binary(b)
        } else if index == self.common_prefix - 1 {
            // Case 4: split on the last bit.
            self.common_prefix -= 1;
            let had_child = self.child.is_some();
            b.children[bit as usize] = self.child.take();
            if had_child {
                b.children[1 - bit as usize] = Some(new_child);
            }
            self.child = Some(Node::Binary(b));
            Node::Unary(self)
        } else {
            // Case 5: split on an interior bit.
            let original_decided_prefix = self.decided_prefix;
            self.decided_prefix = index + 1;
            let cloned_snow = self.snow.clone_instance();
            let preference = self.preference;
            b.children[1 - bit as usize] = Some(new_child);
            b.children[bit as usize] = Some(Node::Unary(self));
            Node::Unary(Box::new(UnaryNode {
                preference,
                decided_prefix: original_decided_prefix,
                common_prefix: index,
                snow: cloned_snow,
                should_reset: false,
                child: Some(Node::Binary(b)),
            }))
        }
    }

    fn record_poll(
        mut self: Box<Self>,
        factory: &F,
        params: &Parameters,
        votes: &Bag<Id>,
        reset: bool,
    ) -> (Node<F>, bool) {
        // All votes have the same bits in [decided_prefix, common_prefix) as
        // the preference (guaranteed by the caller's filtering).

        // If my parent didn't get enough votes, then neither did I.
        if reset {
            self.snow.record_unsuccessful_poll();
            self.should_reset = true; // Reset my child too.
        }

        let num_votes = votes.len() as u32;
        if num_votes < params.alpha_preference {
            self.snow.record_unsuccessful_poll();
            self.should_reset = true;
            return (Node::Unary(self), false);
        }

        self.snow.record_poll(num_votes);

        if let Some(child) = self.child.take() {
            // common_prefix == child.decided_prefix() (beta1 <= beta2), so no
            // further filtering is needed before passing votes down.
            let (new_child, _) = child.record_poll(factory, params, votes, self.should_reset);
            if self.snow.finalized() {
                // If I'm now decided, return my child.
                return (new_child, true);
            }
            // The child's preference may have changed.
            self.preference = new_child.preference();
            self.child = Some(new_child);
        }
        // Votes passed to the child; no need to reset.
        self.should_reset = false;
        (Node::Unary(self), true)
    }
}

/// A node handling a single binary snow instance deciding on one bit.
struct BinaryNode<F: Factory> {
    /// The preferred choice at each branch.
    preferences: [Id; 2],
    /// The bit index this node decides on (range `[0, 256)`).
    bit: i32,
    /// The binary decision instance.
    snow: F::Binary,
    /// Falter continuation per child branch.
    should_reset: [bool; 2],
    /// The (possibly absent) children voting on the next bits.
    children: [Option<Node<F>>; 2],
}

impl<F: Factory> BinaryNode<F> {
    fn add(mut self: Box<Self>, factory: &F, params: &Parameters, id: Id) -> Node<F> {
        let bit = id.bit(self.bit as usize) as usize;
        if let Some(child) = self.children[bit].take() {
            self.children[bit] = Some(child.add(factory, params, id));
        }
        // If the child is None, the id was already added (or rejected by a prior
        // decision); nothing to do.
        Node::Binary(self)
    }

    fn record_poll(
        mut self: Box<Self>,
        factory: &F,
        params: &Parameters,
        votes: &Bag<Id>,
        reset: bool,
    ) -> (Node<F>, bool) {
        // Split the votes into bit-0 votes and bit-1 votes.
        let bit_index = self.bit as usize;
        let split1 = filter_bag(votes, |id| id.bit(bit_index) == 1);
        let split0 = filter_bag(votes, |id| id.bit(bit_index) == 0);

        let mut bit = 0usize;
        // Only care which bit is set if a successful poll can happen.
        if split1.len() as u32 >= params.alpha_preference {
            bit = 1;
        }

        if reset {
            self.snow.record_unsuccessful_poll();
            self.should_reset[bit] = true;
            // 1-bit is set below regardless.
        }
        self.should_reset[1 - bit] = true; // They didn't reach the threshold.

        let pruned_votes = if bit == 1 { split1 } else { split0 };
        let num_votes = pruned_votes.len() as u32;
        if num_votes < params.alpha_preference {
            self.snow.record_unsuccessful_poll();
            self.should_reset[bit] = true;
            return (Node::Binary(self), false);
        }

        self.snow.record_poll(num_votes, bit as u8);

        if let Some(child) = self.children[bit].take() {
            let (new_child, _) =
                child.record_poll(factory, params, &pruned_votes, self.should_reset[bit]);
            if self.snow.finalized() {
                // Decided due to this poll, on `bit`.
                return (new_child, true);
            }
            self.preferences[bit] = new_child.preference();
            self.children[bit] = Some(new_child);
        }
        self.should_reset[bit] = false; // Reset passed down.
        (Node::Binary(self), true)
    }
}

/// Returns a new bag containing only the elements (with their counts) for which
/// `keep` returns true (Go `bag.Bag.Filter`).
fn filter_bag<P: Fn(&Id) -> bool>(votes: &Bag<Id>, keep: P) -> Bag<Id> {
    let mut out = Bag::new();
    for id in votes.list() {
        if keep(&id) {
            let count = votes.count(&id);
            out.add_count(id, count);
        }
    }
    out
}
