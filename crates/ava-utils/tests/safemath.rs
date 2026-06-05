// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use assert_matches::assert_matches;
use ava_utils::error::Error;
use ava_utils::math;

#[test]
fn safemath_checked() {
    assert_eq!(math::add(2u64, 3).unwrap(), 5);
    assert_matches!(math::add(u64::MAX, 1u64), Err(Error::Overflow));

    assert_eq!(math::sub(5u64, 3).unwrap(), 2);
    assert_matches!(math::sub(0u64, 1u64), Err(Error::Underflow));

    assert_eq!(math::mul(4u64, 5).unwrap(), 20);
    assert_matches!(math::mul(u64::MAX, 2u64), Err(Error::Overflow));

    assert_eq!(math::abs_diff(3u64, 10), 7);
    assert_eq!(math::abs_diff(10u64, 3), 7);

    assert_eq!(math::max_uint::<u64>(), u64::MAX);
    assert_eq!(math::max_uint::<u32>(), u32::MAX);
}
