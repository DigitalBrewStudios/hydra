//! Cancelling builds whose client exits mid-build.
//!
//! A daemon build request blocks the client for the whole build, and
//! the nix daemon protocol gives the client no way to say "stop". What
//! it cannot avoid doing is closing the connection: `nix-store
//! --realise` killed with Ctrl+C takes its socket with it. This module
//! watches for that — Nix's own daemon does the same with its
//! `MonitorFdHup` — and hands the signal to a per-connection
//! canceller, which marks the builds the connection filed as
//! `Cancelled`. The schema's `BuildCancelled` trigger then wakes the
//! queue runner, which aborts whatever it was building for those rows.
//!
//! The pieces, in the order a build flows through them:
//!
//! * [`HandshakeOnDisconnect`] (implemented by [`crate::handler`])
//!   creates the connection's [`InflightBuilds`] registry and starts
//!   its canceller task, which waits on the `gone` signal channel.
//! * The server spawns a [`DisconnectWatcher`] around every build
//!   request: a thread that polls the client socket for a hangup.
//! * A watcher fires `gone`; the canceller sees it and cancels what
//!   the connection is still waiting on, while the request loop
//!   abandons the response.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use nix::poll::{PollFd, PollFlags};
use nix::sys::socket::{MsgFlags, recv};
use tokio::sync::watch;

use harmonia_protocol::daemon::{DaemonResult, HandshakeDaemonStore, ResultLog};

use db::models::BuildID;

use crate::waiter::BuildWaiter;

/// How often the watcher thread re-checks its stop flag while the
/// client socket is quiet.
const POLL_INTERVAL_MS: u16 = 250;

/// Watches a client connection for the client closing it.
///
/// A dedicated thread polls a duplicate of the client socket for
/// readability and then peeks one byte: a zero-length peek is what a
/// closed connection looks like. Peeking — not reading — leaves any
/// protocol bytes alone, and the duplicate keeps the file description
/// alive for the thread no matter what the async connection halves do.
#[derive(Debug)]
pub(crate) struct DisconnectWatcher {
    /// Set to stop the thread; also set on drop.
    stop: Arc<AtomicBool>,
}

impl DisconnectWatcher {
    /// Watch `fd`, a duplicate of the client socket, and signal `gone`
    /// when the client exits. The returned handle stops the thread
    /// when dropped.
    pub(crate) fn spawn(fd: &OwnedFd, gone: Arc<watch::Sender<bool>>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let Ok(watch_fd) = fd.try_clone() else {
            tracing::warn!("cannot watch the client socket for exits");
            stop.store(true, Ordering::Release);
            return Self { stop };
        };
        let thread_stop = stop.clone();
        let spawned = std::thread::Builder::new()
            .name("hydra-ad-hoc-hangup".into())
            .spawn(move || watch_for_hangup(watch_fd, thread_stop, gone));
        if let Err(e) = spawned {
            tracing::warn!("cannot start the client hangup watcher: {e}");
            stop.store(true, Ordering::Release);
        }
        Self { stop }
    }

    /// A watcher that never fires, for connections whose socket could
    /// not be duplicated for watching.
    pub(crate) fn dead() -> Self {
        let stop = Arc::new(AtomicBool::new(true));
        Self { stop }
    }
}

impl Drop for DisconnectWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// The watcher thread: signal `gone` once the client socket reports a
/// hangup, exit when `stop` is set.
fn watch_for_hangup(fd: OwnedFd, stop: Arc<AtomicBool>, gone: Arc<watch::Sender<bool>>) {
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let mut fds = [PollFd::new(fd.as_fd(), PollFlags::POLLIN)];
        match nix::poll::poll(&mut fds, POLL_INTERVAL_MS) {
            Ok(0) => continue, // quiet; re-check `stop`
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => {
                // e.g. EBADF once the async connection halves are
                // closed; the connection has ended either way.
                tracing::debug!("client hangup poll failed: {e}");
                return;
            }
        }
        // The socket is readable: either the client sent something or
        // it closed the connection. A zero-length peek distinguishes
        // the two without consuming anything.
        let mut byte = [0u8; 1];
        match recv(fd.as_raw_fd(), &mut byte, MsgFlags::MSG_PEEK) {
            Ok(0) => {
                tracing::debug!("client exited mid-build");
                let _ = gone.send(true);
                return;
            }
            // The client pipelined an operation while a build runs,
            // which the protocol does not do. Stop watching rather
            // than spinning on a socket that stays readable until the
            // request loop drains it.
            Ok(_) => {
                tracing::debug!("client sent data mid-build; hangup watch stops");
                return;
            }
            Err(nix::errno::Errno::EAGAIN) => continue, // spurious wakeup
            Err(e) => {
                tracing::debug!("client hangup peek failed: {e}");
                let _ = gone.send(true);
                return;
            }
        }
    }
}

/// Resolves once the client is gone: a hangup watcher fired, or every
/// sender dropped, meaning the connection ended for another reason.
pub(crate) async fn wait_client_gone(gone: &mut watch::Receiver<bool>) -> bool {
    if *gone.borrow_and_update() {
        return true;
    }
    match gone.changed().await {
        Ok(()) => *gone.borrow_and_update(),
        // All senders dropped without a hangup being seen: the
        // connection is over just the same.
        Err(_) => true,
    }
}

/// The builds one connection has filed that may still be unfinished.
///
/// [`crate::handler`] registers a build before its row is committed,
/// and removes it once the build's fate is known. When the connection
/// ends, [`InflightBuilds::cancel_all`] marks whatever is left as
/// `Cancelled`, so the queue runner stops building for a client that
/// is no longer there. Clones share the same backing state, so the
/// connection's request handling and its canceller task agree.
#[derive(Clone, Debug)]
pub(crate) struct InflightBuilds {
    inner: Arc<InflightInner>,
}

#[derive(Debug)]
struct InflightInner {
    ids: tokio::sync::Mutex<Vec<BuildID>>,
    db: db::Database,
    waiter: BuildWaiter,
}

impl InflightBuilds {
    pub(crate) fn new(db: db::Database, waiter: BuildWaiter) -> Self {
        Self {
            inner: Arc::new(InflightInner {
                ids: tokio::sync::Mutex::new(Vec::new()),
                db,
                waiter,
            }),
        }
    }

    /// Track `build_id` until its fate is known. Call before the build
    /// row is committed, so a connection that dies in between cannot
    /// leave the build untracked.
    pub(crate) async fn register(&self, build_id: BuildID) {
        self.inner.ids.lock().await.push(build_id);
    }

    /// The build's fate is known; nothing left to cancel.
    pub(crate) async fn remove(&self, build_id: BuildID) {
        self.inner.ids.lock().await.retain(|&id| id != build_id);
    }

    /// Mark every still-unfinished build this connection filed as
    /// `Cancelled`. Rows the queue runner finished first are left
    /// alone.
    pub(crate) async fn cancel_all(&self) {
        let ids = std::mem::take(&mut *self.inner.ids.lock().await);
        if ids.is_empty() {
            return;
        }
        tracing::info!(
            count = ids.len(),
            "client exited; cancelling its unfinished builds"
        );
        for build_id in ids {
            let db = self.inner.db.clone();
            if let Err(e) = cancel_abandoned_build(&db, build_id).await {
                // The row stays unfinished and the build keeps
                // running, as it would have before the client exited;
                // hydra's own cancellation can still get to it.
                tracing::error!(build_id, "cannot cancel the abandoned build: {e}");
            }
            // The `build_finished` notification inside
            // `cancel_abandoned_build` wakes the waiter in the normal
            // case; this covers the races it cannot (the build was
            // already finished by the queue runner, say).
            self.inner.waiter.forget(build_id).await;
        }
    }

    /// Start the canceller task for this connection. It fires when a
    /// hangup watcher saw the client exit, or when every sender of
    /// `gone` dropped: the connection ended for another reason, and
    /// whatever is still tracked was left behind just the same.
    pub(crate) fn spawn_canceller(&self, gone: watch::Receiver<bool>) {
        let this = self.clone();
        tokio::spawn(async move {
            if wait_client_gone(&mut gone.clone()).await {
                this.cancel_all().await;
            }
        });
    }
}

async fn cancel_abandoned_build(db: &db::Database, build_id: BuildID) -> Result<(), db::Error> {
    let mut conn = db.get().await?;
    crate::queries::cancel_builds(conn.raw(), &[build_id]).await?;
    Ok(())
}

/// Handshake that hooks up the client-exit machinery.
///
/// The server calls this instead of
/// [`HandshakeDaemonStore::handshake`] for every connection: the
/// returned store tracks the builds its connection files and cancels
/// them when the client exits before they finish.
pub(crate) trait HandshakeOnDisconnect: HandshakeDaemonStore {
    fn handshake_on_disconnect(
        self,
        gone: watch::Receiver<bool>,
    ) -> impl ResultLog<Output = DaemonResult<Self::Store>> + Send;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sqlx::Connection as _;
    use tokio::io::AsyncWriteExt as _;
    use tokio::net::UnixStream;

    use super::*;
    use db::models::BuildStatus;

    /// A client and its daemon end, plus the hangup watcher running on
    /// the daemon end, a receiver for its `gone` signal, and a handle
    /// on the channel itself — the connection task holds one for its
    /// whole life, so the tests must too.
    #[allow(clippy::type_complexity)]
    async fn watched_pair() -> (
        UnixStream,
        UnixStream,
        DisconnectWatcher,
        watch::Receiver<bool>,
        Arc<watch::Sender<bool>>,
    ) {
        let (client, daemon) = UnixStream::pair().unwrap();
        let (gone_tx, gone_rx) = watch::channel(false);
        let gone_tx = Arc::new(gone_tx);
        // The server dups its own end before anything consumes it, and
        // hands out receivers before any watcher can fire.
        let fd = nix::unistd::dup(&daemon).unwrap();
        let watcher = DisconnectWatcher::spawn(&fd, gone_tx.clone());
        (client, daemon, watcher, gone_rx, gone_tx)
    }

    /// True if `wait_client_gone` resolves within the deadline.
    async fn gone_within(gone: &mut watch::Receiver<bool>, within: Duration) -> bool {
        tokio::time::timeout(within, wait_client_gone(gone))
            .await
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn detects_client_exit() {
        let (client, _daemon, _watcher, mut gone, _gone_tx) = watched_pair().await;
        // The client exits: its end closes, so the watched daemon end
        // sees EOF.
        drop(client);
        assert!(gone_within(&mut gone, Duration::from_secs(5)).await);
    }

    #[tokio::test]
    async fn quiet_while_client_is_alive() {
        let (mut client, _daemon, _watcher, mut gone, _gone_tx) = watched_pair().await;
        // Traffic on the connection without a hangup must not fire.
        client.write_all(b"hello").await.unwrap();
        assert!(!gone_within(&mut gone, Duration::from_millis(750)).await);
        // The watcher stopped on the data, and stays stopped: the
        // client exiting afterwards must not fire either.
        drop(client);
        assert!(!gone_within(&mut gone, Duration::from_millis(750)).await);
    }

    async fn db_setup() -> (test_utils::TestPg, db::Database) {
        let (pg, _pool) = test_utils::TestPg::new().await;
        let db = db::Database::new(&pg.url(), 2).await.unwrap();
        (pg, db)
    }

    /// File a build the way the daemon does, so there is a row the
    /// canceller can act on.
    async fn submit_build(db: &db::Database) -> BuildID {
        let submitter = crate::submit::AdhocSubmitter::new(db.clone()).await.unwrap();
        let mut conn = db.get().await.unwrap();
        let mut tx = conn.raw().begin().await.unwrap();
        let id = submitter
            .submit(
                &mut tx,
                crate::submit::BuildRequest {
                    drv_path: "/nix/store/foo.drv",
                    nix_name: "hello",
                    system: "x86_64-linux",
                },
            )
            .await
            .unwrap();
        tx.commit().await.unwrap();
        id
    }

    async fn build_row(db: &db::Database, id: BuildID) -> (i32, Option<i32>) {
        let mut conn = db.get().await.unwrap();
        let row = sqlx::query!("SELECT finished, buildStatus FROM Builds WHERE id = $1", id)
            .fetch_one(conn.raw())
            .await
            .unwrap();
        (row.finished, row.buildstatus)
    }

    #[tokio::test]
    async fn cancel_all_cancels_tracked_builds() {
        let (_pg, db) = db_setup().await;
        let waiter = BuildWaiter::start(&db).await.unwrap();
        let inflight = InflightBuilds::new(db.clone(), waiter);
        let id = submit_build(&db).await;
        inflight.register(id).await;

        inflight.cancel_all().await;

        let (finished, status) = build_row(&db, id).await;
        assert_eq!((finished, status), (1, Some(BuildStatus::Cancelled as i32)));
    }

    #[tokio::test]
    async fn removed_builds_are_not_cancelled() {
        let (_pg, db) = db_setup().await;
        let waiter = BuildWaiter::start(&db).await.unwrap();
        let inflight = InflightBuilds::new(db.clone(), waiter);
        let id = submit_build(&db).await;
        inflight.register(id).await;
        // `run_build` does this once the build's fate is known.
        inflight.remove(id).await;

        inflight.cancel_all().await;

        let (finished, _status) = build_row(&db, id).await;
        assert_eq!(finished, 0, "a finished build needs no cancellation");
    }

    #[tokio::test]
    async fn cancel_all_on_empty_registry_is_quiet() {
        let (_pg, db) = db_setup().await;
        let waiter = BuildWaiter::start(&db).await.unwrap();
        let inflight = InflightBuilds::new(db.clone(), waiter);
        inflight.cancel_all().await;
    }
}


