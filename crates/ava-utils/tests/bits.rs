// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use ava_utils::bits::Bits;

#[test]
fn bits_set_algebra() {
    let mut a = Bits::new();
    a.add(0);
    a.add(2);
    a.add(64);
    assert!(a.contains(0));
    assert!(a.contains(64));
    assert!(!a.contains(1));
    assert_eq!(a.len(), 3);

    let mut b = Bits::new();
    b.add(2);
    b.add(3);

    let union = Bits::union(&a, &b);
    assert_eq!(union.len(), 4); // {0,2,3,64}
    assert!(union.contains(3));

    let inter = Bits::intersection(&a, &b);
    assert_eq!(inter.len(), 1); // {2}
    assert!(inter.contains(2));

    let diff = Bits::difference(&a, &b);
    assert_eq!(diff.len(), 2); // {0,64}
    assert!(diff.contains(0));
    assert!(diff.contains(64));
    assert!(!diff.contains(2));

    // remove
    let mut c = a.clone();
    c.remove(64);
    assert!(!c.contains(64));
    assert_eq!(c.len(), 2);

    // big-endian Bytes round-trip
    let bytes = a.bytes();
    let restored = Bits::from_bytes(&bytes);
    assert_eq!(restored, a);

    // empty
    let empty = Bits::new();
    assert_eq!(empty.len(), 0);
    assert!(empty.bytes().is_empty());
    assert_eq!(Bits::from_bytes(&[]), empty);
}
