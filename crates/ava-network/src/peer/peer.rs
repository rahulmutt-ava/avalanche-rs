// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `Peer` actor — three tokio tasks per peer sharing an `Arc<Peer>`
//! (`specs/05` §1.1/§1.4/§3.2, `specs/17` §2 #5/#6/#7, §3, §4, §7).
//!
//! Mirrors Go `network/peer/peer.go`'s three goroutines:
//!
//! 1. **read task** (`readMessages`): read a 4-byte BE length, enforce the
//!    `MAX_MESSAGE_SIZE` cap, acquire inbound bytes, read the payload, parse it,
//!    and dispatch. Network ops (`Handshake`/`Ping`/`Pong`/`GetPeerList`/
//!    `PeerList`) are handled inline; all other ops are forwarded to the router
//!    only after the handshake has finished. Each read is wrapped in a
//!    `pong_timeout` deadline (Go's read-deadline reset).
//! 2. **write task** (`writeMessages`): force the `Handshake` as the first
//!    frame, then drain the outbound queue, writing `len || payload` with
//!    vectored I/O (`specs/17` §10).
//! 3. **net-messages task** (`sendNetworkMessages`): `select!` over the command
//!    channel, the ping ticker (`PingFrequency`), and the close token.
//!
//! The last task to finish latches `closed` and the `Network` learns of the
//! disconnect (the `Network` wires `ExternalHandler::disconnected` from the peer
//! set; the actor signals via `closed`).
//!
//! **Locking discipline (`specs/17` §7):** no synchronous lock is held across an
//! `.await`. Mutable handshake state lives behind a `parking_lot::Mutex` that is
//! only locked for short, await-free critical sections; liveness flags are
//! atomics.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};

use ava_crypto::staking::Certificate;
use ava_message::builder::OutboundMsgBuilder;
use ava_message::codec::{MsgBuilder, OutboundMessage};
use ava_message::frame::{MAX_MESSAGE_SIZE, read_msg_len};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::config::PeerConfig;
use crate::peer::handle::{PeerCommand, PeerHandle};
use crate::peer::ip::SignedIp;
use crate::peer::message_queue::MessageQueue;

/// Command-channel capacity. The net-messages task drains commands; the channel
/// only needs to absorb a small burst (Go's `getPeerListChan` is cap 1, sends
/// use the queue directly).
const CMD_CHANNEL_CAP: usize = 16;

/// Whether this side dialed the peer (egress) or accepted it (ingress).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// We dialed the peer (outbound).
    Outbound,
    /// The peer dialed us (inbound).
    Inbound,
}

/// Mutable handshake state, written by the read task during the handshake and
/// read elsewhere. Guarded by a short-lived non-async mutex.
#[derive(Default)]
pub(crate) struct HandshakeState {
    /// The peer's signed IP claim (set on `Handshake`).
    pub(crate) ip: Option<SignedIp>,
    /// The peer's reported application version (set on `Handshake`).
    pub(crate) version: Option<ava_version::Application>,
    /// The subnets the peer tracks (set on `Handshake`).
    pub(crate) tracked_subnets: Vec<Id>,
    /// The transaction id of the peer's verified BLS key (cached so
    /// `should_disconnect` need not re-verify each tick). Populated once the
    /// validator-set source lands (the BLS-PoP re-check); reserved now.
    #[allow(dead_code)]
    pub(crate) txid_of_verified_bls_key: Option<Id>,
}

/// The shared per-peer actor state (`Arc<Peer>` held by all three tasks).
pub struct Peer {
    /// Shared per-peer configuration.
    pub(crate) cfg: Arc<PeerConfig>,
    /// The peer's NodeID (derived from its cert).
    pub(crate) id: NodeId,
    /// The peer's leaf certificate (for signed-IP verification).
    pub(crate) cert: Certificate,
    /// Whether the peer dialed us. Surfaced as `PeerInfo.is_ingress` once the
    /// info/metrics endpoints consume it (M2.20); reserved now.
    #[allow(dead_code)]
    pub(crate) direction: Direction,
    /// The outbound message queue (`specs/05` §3.3).
    pub(crate) queue: Arc<crate::peer::message_queue::ThrottledMessageQueue>,

    /// Set once we have processed the peer's `Handshake`.
    pub(crate) got_handshake: AtomicBool,
    /// Set once the handshake has finished (peer's `PeerList` received).
    pub(crate) finished_handshake: CancellationToken,
    /// What the peer thinks our uptime is (from inbound `Ping`s), `[0,100]`.
    pub(crate) observed_uptime: AtomicU32,
    /// Unix-nanos of the last `Ping` we sent that is awaiting a `Pong`; `0` when
    /// none is outstanding.
    pub(crate) last_ping_sent_nanos: AtomicI64,

    /// Mutable handshake state.
    pub(crate) hs: Mutex<HandshakeState>,

    /// This peer's cancellation token (grandchild of the network token).
    pub(crate) close_token: CancellationToken,
}

impl Peer {
    /// Spawns the three peer tasks over a byte stream `io`, returning a
    /// [`PeerHandle`].
    ///
    /// `parent_token` is the network's cancellation token; the peer derives a
    /// grandchild from it so a network shutdown cancels every peer.
    pub fn spawn<IO>(
        cfg: Arc<PeerConfig>,
        id: NodeId,
        cert: Certificate,
        direction: Direction,
        io: IO,
        parent_token: &CancellationToken,
        tracker: &TaskTracker,
    ) -> PeerHandle
    where
        IO: AsyncRead + AsyncWrite + Send + 'static,
    {
        let close_token = parent_token.child_token();
        let closed = CancellationToken::new();
        let finished_handshake = CancellationToken::new();

        let queue = Arc::new(crate::peer::message_queue::ThrottledMessageQueue::new(
            cfg.outbound_msg_throttler.clone(),
            id,
        ));

        let peer = Arc::new(Peer {
            cfg,
            id,
            cert,
            direction,
            queue: Arc::clone(&queue),
            got_handshake: AtomicBool::new(false),
            finished_handshake: finished_handshake.clone(),
            observed_uptime: AtomicU32::new(0),
            last_ping_sent_nanos: AtomicI64::new(0),
            hs: Mutex::new(HandshakeState::default()),
            close_token: close_token.clone(),
        });

        let (cmd_tx, cmd_rx) = mpsc::channel(CMD_CHANNEL_CAP);

        let (read_half, write_half) = tokio::io::split(io);

        // A countdown across the three tasks: the last one to finish latches
        // `closed`. An `Arc` strong-count drop guard is the idiomatic Rust
        // analogue of Go's `numExecuting` counter (`specs/05` §3.2).
        let drop_guard = Arc::new(CloseGuard {
            closed: closed.clone(),
        });

        // read task (#5)
        {
            let peer = Arc::clone(&peer);
            let guard = Arc::clone(&drop_guard);
            tracker.spawn(async move {
                peer.run_read(read_half).await;
                // Closing the queue lets the write task drain and exit.
                peer.queue.close();
                peer.close_token.cancel();
                drop(guard);
            });
        }

        // write task (#6)
        {
            let peer = Arc::clone(&peer);
            let queue = Arc::clone(&queue);
            let guard = Arc::clone(&drop_guard);
            tracker.spawn(async move {
                peer.run_write(write_half, queue).await;
                peer.close_token.cancel();
                drop(guard);
            });
        }

        // net-messages task (#7)
        {
            let peer = Arc::clone(&peer);
            let guard = Arc::clone(&drop_guard);
            tracker.spawn(async move {
                peer.run_net_messages(cmd_rx).await;
                peer.queue.close();
                peer.close_token.cancel();
                drop(guard);
            });
        }

        drop(drop_guard);

        PeerHandle {
            id,
            cmd_tx,
            finished_handshake,
            closed,
            close_token,
        }
    }

    /// The read task (Go `readMessages`).
    async fn run_read<R>(self: &Arc<Self>, mut read: R)
    where
        R: AsyncRead + Unpin,
    {
        let mb = MsgBuilder::default();
        loop {
            // Read the 4-byte length prefix under the close token + read
            // deadline (`PongTimeout`): a silent peer is dropped.
            let mut len_buf = [0u8; 4];
            let read_len = tokio::select! {
                biased;
                () = self.close_token.cancelled() => return,
                r = tokio::time::timeout(self.cfg.pong_timeout, read.read_exact(&mut len_buf)) => r,
            };
            match read_len {
                Ok(Ok(_)) => {}
                // Timeout or EOF / error: drop the peer.
                _ => return,
            }

            let len = match read_msg_len(len_buf, MAX_MESSAGE_SIZE) {
                Ok(len) => len,
                // Oversized frame is a protocol error: close.
                Err(_) => return,
            };

            // Acquire inbound bytes (never-drop; back-pressures the peer).
            let _permit = {
                let acquired = self
                    .cfg
                    .inbound_msg_throttler
                    .acquire(u64::from(len), self.id, &self.close_token)
                    .await;
                match acquired {
                    Some(p) => p,
                    // Cancelled while blocked → shutting down.
                    None => return,
                }
            };

            let mut payload = vec![0u8; len as usize];
            let read_payload = tokio::select! {
                biased;
                () = self.close_token.cancelled() => return,
                r = tokio::time::timeout(self.cfg.pong_timeout, read.read_exact(&mut payload)) => r,
            };
            match read_payload {
                Ok(Ok(_)) => {}
                _ => return,
            }

            let payload = Bytes::from(payload);
            if self.handle_inbound(&mb, &payload).await.is_err() {
                // A disconnect-reason error: close the connection.
                return;
            }
        }
    }

    /// The write task (Go `writeMessages`): handshake forced first, then drain
    /// the queue.
    async fn run_write<W>(
        self: &Arc<Self>,
        mut write: W,
        queue: Arc<crate::peer::message_queue::ThrottledMessageQueue>,
    ) where
        W: AsyncWrite + Unpin,
    {
        // Force the Handshake as the very first frame (`specs/05` §1.4).
        match self.build_handshake() {
            Ok(handshake) => {
                if self.write_frame(&mut write, &handshake).await.is_err() {
                    self.close_token.cancel();
                    return;
                }
            }
            Err(_) => {
                self.close_token.cancel();
                return;
            }
        }

        loop {
            // Drain anything immediately available first (Go's non-blocking
            // pop), then block for the next message.
            while let Some(msg) = queue.pop_now() {
                if self.write_frame(&mut write, &msg).await.is_err() {
                    self.close_token.cancel();
                    return;
                }
            }
            let _ = write.flush().await;

            let next = tokio::select! {
                biased;
                () = self.close_token.cancelled() => None,
                m = queue.pop() => m,
            };
            match next {
                Some(msg) => {
                    if self.write_frame(&mut write, &msg).await.is_err() {
                        self.close_token.cancel();
                        return;
                    }
                }
                None => return,
            }
        }
    }

    /// Writes one framed message: 4-byte BE length prefix then the payload.
    /// Mirrors Go's vectored `len || payload` write.
    async fn write_frame<W>(&self, write: &mut W, msg: &OutboundMessage) -> io::Result<()>
    where
        W: AsyncWrite + Unpin,
    {
        let len = u32::try_from(msg.bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
        if len > MAX_MESSAGE_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "message exceeds MAX_MESSAGE_SIZE",
            ));
        }
        // One contiguous buffer (len prefix + payload). tokio's split writer is
        // not directly vectored; a single `write_all` of the joined buffer
        // preserves the on-wire framing and avoids a partial-frame interleave.
        let mut framed = Vec::with_capacity(4usize.saturating_add(msg.bytes.len()));
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(&msg.bytes);
        write.write_all(&framed).await
    }

    /// The net-messages task (Go `sendNetworkMessages`).
    async fn run_net_messages(self: &Arc<Self>, mut cmd_rx: mpsc::Receiver<PeerCommand>) {
        let mut ticker = tokio::time::interval(self.cfg.ping_frequency);
        // Skip the immediate first tick so we don't ping before the handshake.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await;

        loop {
            tokio::select! {
                biased;
                () = self.close_token.cancelled() => return,
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(PeerCommand::Send(msg)) => {
                            self.queue.push(msg);
                        }
                        Some(PeerCommand::GetPeerList) => {
                            self.on_get_peer_list_trigger();
                        }
                        Some(PeerCommand::Close) | None => return,
                    }
                }
                _ = ticker.tick() => {
                    if self.on_tick().is_err() {
                        return;
                    }
                }
            }
        }
    }

    /// Re-check compatibility and send a `Ping` carrying our uptime
    /// (`specs/05` §1.5). Returns `Err` if the peer should now be dropped.
    fn on_tick(self: &Arc<Self>) -> crate::Result<()> {
        if self.finished_handshake.is_cancelled() {
            self.should_disconnect()?;
            self.send_ping();
        }
        Ok(())
    }

    /// Hook for the `GetPeerList` trigger (peer-list gossip lands in M2.17).
    fn on_get_peer_list_trigger(self: &Arc<Self>) {}
}

impl Peer {
    /// Builds our outbound `Handshake` (the forced-first frame) using the
    /// config's identity, IP signer, version, and tracked subnets
    /// (`specs/05` §1.4).
    pub(crate) fn build_handshake(&self) -> crate::Result<OutboundMessage> {
        let signed = self
            .cfg
            .ip_signer
            .get_signed_ip(self.cfg.my_ip.ip(), self.cfg.my_ip.port())?;

        let my_time = self.cfg.clock.unix();
        let upgrade_time = self
            .cfg
            .version_compatibility
            .upgrade_time
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let msg = self.cfg.creator.handshake(
            self.cfg.network_id,
            my_time,
            self.cfg.my_ip,
            &self.cfg.my_version.name,
            self.cfg.my_version.major,
            self.cfg.my_version.minor,
            self.cfg.my_version.patch,
            upgrade_time,
            signed.unsigned.timestamp,
            signed.tls_signature(),
            signed.bls_signature_bytes(),
            &self.cfg.my_tracked_subnets,
            &self.cfg.my_supported_acps,
            &self.cfg.my_objected_acps,
            &[],
            &[],
            true,
        )?;
        Ok(msg)
    }

    /// Dispatch one inbound parsed frame. Network-level ops are handled inline;
    /// every other op is forwarded to the router, but only after the handshake
    /// has finished (`specs/05` §3.2/§3.6). Returns `Err` for a disconnect.
    pub(crate) async fn handle_inbound(
        self: &Arc<Self>,
        mb: &MsgBuilder,
        payload: &Bytes,
    ) -> crate::Result<()> {
        use ava_message::proto::p2p::message::Message as M;

        let inbound = mb.parse_inbound(payload)?;
        match inbound.message {
            M::Handshake(h) => self.handle_handshake(h),
            M::PeerList(pl) => self.handle_peer_list(pl),
            M::GetPeerList(gpl) => self.handle_get_peer_list(gpl),
            M::Ping(ping) => self.handle_ping(ping),
            M::Pong(pong) => self.handle_pong(pong),
            other => {
                // Non-handshake op: forward to the router only after the
                // handshake has finished; drop (ignore) otherwise.
                if self.finished_handshake.is_cancelled() {
                    let forwarded = ava_message::codec::InboundMessage {
                        op: inbound.op,
                        message: other,
                        expiration: inbound.expiration,
                        bytes_saved_compression: inbound.bytes_saved_compression,
                    };
                    self.cfg
                        .router
                        .handle_inbound(&self.close_token, forwarded)
                        .await;
                }
                Ok(())
            }
        }
    }

    /// Send a `Ping` carrying our uptime `[0,100]` (`specs/05` §1.5). The
    /// uptime calculator is not yet wired, so we report `0`.
    fn send_ping(self: &Arc<Self>) {
        let uptime = self.observed_uptime.load(Ordering::Relaxed).min(100);
        if let Ok(ping) = self.cfg.creator.ping(uptime) {
            // Record the send time (nanos) so the Pong can compute the RTT.
            let now = unix_nanos(&*self.cfg.clock);
            self.last_ping_sent_nanos.store(now, Ordering::Relaxed);
            self.queue.push(ping);
        }
    }

    /// Re-check version compatibility (`specs/26` §3.1). Returns `Err` to drop
    /// the peer. Re-running `is_compatible` with the injected clock on each tick
    /// is the fork-boundary safety mechanism: a peer that was compatible under
    /// the pre-upgrade floor becomes incompatible the instant the clock crosses
    /// `upgrade_time`, and is dropped on the next tick.
    pub(crate) fn should_disconnect(self: &Arc<Self>) -> crate::Result<()> {
        let version = self.hs.lock().version.clone();
        if let Some(v) = version
            && !self.is_compatible(&v)
        {
            return Err(crate::error::Error::IncompatibleVersion(v.display()));
        }
        // The BLS-PoP re-check (caching `txid_of_verified_bls_key`) is wired
        // once the validator-set source lands; the signed-IP TLS check already
        // ran at handshake.
        Ok(())
    }
}

/// Current time in Unix nanoseconds from an injected clock (seconds resolution),
/// saturating into an `i64` for the RTT bookkeeping.
fn unix_nanos(clock: &dyn crate::peer::ip_signer::Clock) -> i64 {
    let secs = clock.unix();
    i64::try_from(secs.saturating_mul(1_000_000_000)).unwrap_or(i64::MAX)
}

/// A drop guard whose last surviving clone latches `closed` (Go `numExecuting`
/// reaching zero → `Network.disconnected`).
struct CloseGuard {
    closed: CancellationToken,
}

impl Drop for CloseGuard {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}
