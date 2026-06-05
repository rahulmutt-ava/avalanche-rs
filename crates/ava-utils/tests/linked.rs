// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use ava_utils::linked::LinkedHashmap;

#[test]
fn linked_move_to_back() {
    let mut m: LinkedHashmap<u32, &str> = LinkedHashmap::new();
    m.put(1, "a");
    m.put(2, "b");
    m.put(3, "c");
    assert_eq!(m.len(), 3);
    assert_eq!(m.get(&2), Some(&"b"));

    // insertion order preserved
    let order: Vec<u32> = m.keys().copied().collect();
    assert_eq!(order, vec![1, 2, 3]);

    // re-Put of an existing key moves it to back
    m.put(1, "a2");
    let order: Vec<u32> = m.keys().copied().collect();
    assert_eq!(order, vec![2, 3, 1]);
    assert_eq!(m.get(&1), Some(&"a2"));

    // oldest / newest
    assert_eq!(m.oldest(), Some((&2, &"b")));
    assert_eq!(m.newest(), Some((&1, &"a2")));

    // delete
    let removed = m.delete(&3);
    assert_eq!(removed, Some("c"));
    let order: Vec<u32> = m.keys().copied().collect();
    assert_eq!(order, vec![2, 1]);
    assert_eq!(m.len(), 2);
}
