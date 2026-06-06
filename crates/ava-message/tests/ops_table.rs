// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M2.2 — the `Op` opcode enum + classification sets, byte-exact with Go
//! `message/ops.go` (specs/05 §1.2/§2.2).

use ava_message::ops::{failed_to_response_ops, unrequested_ops, Op};
use ava_message::proto::p2p;

#[test]
fn op_values_and_strings_match_go() {
    // iota ordering from message/ops.go.
    assert_eq!(Op::Ping as u8, 0);
    assert_eq!(Op::Pong as u8, 1);
    assert_eq!(Op::Handshake as u8, 2);
    assert_eq!(Op::GetPeerList as u8, 3);
    assert_eq!(Op::PeerList as u8, 4);
    assert_eq!(Op::Simplex as u8, 35);

    // String() names from ops.go.
    assert_eq!(Op::Handshake.as_str(), "handshake");
    assert_eq!(Op::GetPeerList.as_str(), "get_peerlist");
    assert_eq!(Op::Ping.as_str(), "ping");
    assert_eq!(Op::PeerList.as_str(), "peerlist");
    assert_eq!(Op::AppError.as_str(), "app_error");

    // ToOp: oneof variant -> Op.
    let m = p2p::message::Message::Handshake(p2p::Handshake::default());
    assert_eq!(Op::of(&m).unwrap(), Op::Handshake);
    let m = p2p::message::Message::Ping(p2p::Ping { uptime: 0 });
    assert_eq!(Op::of(&m).unwrap(), Op::Ping);
}

#[test]
fn classification_sets_match_go() {
    let u = unrequested_ops();
    for op in [
        Op::GetAcceptedFrontier,
        Op::GetAccepted,
        Op::GetAncestors,
        Op::Get,
        Op::PushQuery,
        Op::PullQuery,
        Op::AppRequest,
        Op::AppGossip,
        Op::GetStateSummaryFrontier,
        Op::GetAcceptedStateSummary,
        Op::Simplex,
    ] {
        assert!(u.contains(&op), "unrequested_ops missing {op:?}");
    }
    assert_eq!(u.len(), 11);
    // Sanity: a response op is NOT unrequested.
    assert!(!u.contains(&Op::Put));

    let f = failed_to_response_ops();
    assert_eq!(f.get(&Op::GetFailed), Some(&Op::Put));
    assert_eq!(f.get(&Op::QueryFailed), Some(&Op::Chits));
    assert_eq!(f.get(&Op::AppError), Some(&Op::AppResponse));
    assert_eq!(f.get(&Op::GetStateSummaryFrontierFailed), Some(&Op::StateSummaryFrontier));
    assert_eq!(f.get(&Op::GetAcceptedStateSummaryFailed), Some(&Op::AcceptedStateSummary));
    assert_eq!(f.get(&Op::GetAcceptedFrontierFailed), Some(&Op::AcceptedFrontier));
    assert_eq!(f.get(&Op::GetAcceptedFailed), Some(&Op::Accepted));
    assert_eq!(f.get(&Op::GetAncestorsFailed), Some(&Op::Ancestors));
    assert_eq!(f.len(), 8);
}
