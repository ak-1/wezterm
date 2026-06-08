use crate::sessionhandler::{PduSender, SessionHandler};
use anyhow::Context;
use async_ossl::AsyncSslStream;
use codec::{DecodedPdu, Pdu};
use futures::FutureExt;
use mux::{Mux, MuxNotification};
use smol::prelude::*;
use smol::Async;
use std::net::Shutdown;
use std::sync::{Arc, Mutex};
use wezterm_uds::UnixStream;

#[cfg(unix)]
pub trait AsRawDesc: std::os::unix::io::AsRawFd + std::os::fd::AsFd {}
#[cfg(windows)]
pub trait AsRawDesc: std::os::windows::io::AsRawSocket + std::os::windows::io::AsSocket {}

impl AsRawDesc for UnixStream {}
impl AsRawDesc for AsyncSslStream {}

#[derive(Debug)]
enum Item {
    Notif(MuxNotification),
    WritePdu(DecodedPdu),
    Readable,
    /// Sent to the blocking writer thread to ask it to stop (see `serve_local`).
    Shutdown,
}

pub async fn process<T>(stream: T) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: AsRawDesc,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    let stream = smol::Async::new(stream)?;
    process_async(stream).await
}

pub async fn process_async<T>(mut stream: Async<T>) -> anyhow::Result<()>
where
    T: 'static,
    T: std::io::Read,
    T: std::io::Write,
    T: std::fmt::Debug,
    T: async_io::IoSafe,
{
    log::trace!("process_async called");

    let (item_tx, item_rx) = smol::channel::unbounded::<Item>();

    let pdu_sender = PduSender::new({
        let item_tx = item_tx.clone();
        move |pdu| {
            item_tx
                .try_send(Item::WritePdu(pdu))
                .map_err(|e| anyhow::anyhow!("{:?}", e))
        }
    });
    let mut handler = SessionHandler::new(pdu_sender);

    {
        let mux = Mux::get();
        let tx = item_tx.clone();
        mux.subscribe(move |n| tx.try_send(Item::Notif(n)).is_ok());
    }

    loop {
        let rx_msg = item_rx.recv();
        let wait_for_read = stream.readable().map(|_| Ok(Item::Readable));

        match smol::future::or(rx_msg, wait_for_read).await {
            Ok(Item::Readable) => {
                let decoded = match Pdu::decode_async(&mut stream, None).await {
                    Ok(data) => data,
                    Err(err) => {
                        if let Some(err) = err.root_cause().downcast_ref::<std::io::Error>() {
                            if err.kind() == std::io::ErrorKind::UnexpectedEof {
                                // Client disconnected: no need to make a noise
                                return Ok(());
                            }
                        }
                        return Err(err).context("reading Pdu from client");
                    }
                };
                handler.process_one(decoded);
            }
            Ok(Item::WritePdu(decoded)) => {
                match decoded.pdu.encode_async(&mut stream, decoded.serial).await {
                    Ok(()) => {}
                    Err(err) => {
                        if let Some(err) = err.root_cause().downcast_ref::<std::io::Error>() {
                            if err.kind() == std::io::ErrorKind::BrokenPipe {
                                // Client disconnected: no need to make a noise
                                return Ok(());
                            }
                        }
                        return Err(err).context("encoding PDU to client");
                    }
                };
                match stream.flush().await {
                    Ok(()) => {}
                    Err(err) => {
                        if err.kind() == std::io::ErrorKind::BrokenPipe {
                            // Client disconnected: no need to make a noise
                            return Ok(());
                        }
                        return Err(err).context("flushing PDU to client");
                    }
                }
            }
            Ok(Item::Notif(MuxNotification::PaneOutput(pane_id))) => {
                handler.schedule_pane_push(pane_id);
            }
            Ok(Item::Notif(MuxNotification::PaneAdded(_pane_id))) => {}
            Ok(Item::Notif(MuxNotification::PaneRemoved(pane_id))) => {
                Pdu::PaneRemoved(codec::PaneRemoved { pane_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::Alert { pane_id, alert })) => {
                {
                    let per_pane = handler.per_pane(pane_id);
                    let mut per_pane = per_pane.lock().unwrap();
                    per_pane.notifications.push(alert);
                }
                handler.schedule_pane_push(pane_id);
            }
            Ok(Item::Notif(MuxNotification::SaveToDownloads { .. })) => {}
            Ok(Item::Notif(MuxNotification::AssignClipboard {
                pane_id,
                selection,
                clipboard,
            })) => {
                Pdu::SetClipboard(codec::SetClipboard {
                    pane_id,
                    clipboard,
                    selection,
                })
                .encode_async(&mut stream, 0)
                .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabAddedToWindow { tab_id, window_id })) => {
                Pdu::TabAddedToWindow(codec::TabAddedToWindow { tab_id, window_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WindowRemoved(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowCreated(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowInvalidated(_window_id))) => {}
            Ok(Item::Notif(MuxNotification::WindowWorkspaceChanged(window_id))) => {
                let workspace = {
                    let mux = Mux::get();
                    mux.get_window(window_id)
                        .map(|w| w.get_workspace().to_string())
                };
                if let Some(workspace) = workspace {
                    Pdu::WindowWorkspaceChanged(codec::WindowWorkspaceChanged {
                        window_id,
                        workspace,
                    })
                    .encode_async(&mut stream, 0)
                    .await?;
                    stream.flush().await.context("flushing PDU to client")?;
                }
            }
            Ok(Item::Notif(MuxNotification::PaneFocused(pane_id))) => {
                Pdu::PaneFocused(codec::PaneFocused { pane_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabResized(tab_id))) => {
                Pdu::TabResized(codec::TabResized { tab_id })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::TabTitleChanged { tab_id, title })) => {
                Pdu::TabTitleChanged(codec::TabTitleChanged { tab_id, title })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WindowTitleChanged { window_id, title })) => {
                Pdu::WindowTitleChanged(codec::WindowTitleChanged { window_id, title })
                    .encode_async(&mut stream, 0)
                    .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::WorkspaceRenamed {
                old_workspace,
                new_workspace,
            })) => {
                Pdu::RenameWorkspace(codec::RenameWorkspace {
                    old_workspace,
                    new_workspace,
                })
                .encode_async(&mut stream, 0)
                .await?;
                stream.flush().await.context("flushing PDU to client")?;
            }
            Ok(Item::Notif(MuxNotification::ActiveWorkspaceChanged(_))) => {}
            Ok(Item::Notif(MuxNotification::Empty)) => {}
            // Only produced on the blocking `serve_local` path; never sent here.
            Ok(Item::Shutdown) => {}
            Err(err) => {
                log::error!("process_async Err {}", err);
                return Ok(());
            }
        }
    }
}

/// Serve a local (unix socket) client using blocking I/O on dedicated threads,
/// rather than the async `process_async` path.
///
/// `process_async` waits for inbound data with async-io's `readable()`, which is
/// backed by an edge/oneshot epoll registration (`polling` uses `PollMode::Oneshot`).
/// Recreating that future every loop iteration can lose a readiness edge: a byte
/// that arrives just as the loop re-arms produces no wakeup, so the connection --
/// and, because every pane's output push flows through these per-client loops, the
/// whole server -- stalls until an unrelated socket edge (e.g. the next keypress)
/// happens to wake it. (Confirmed by an strace showing the mux-server idle in
/// `epoll_pwait` for 60s with a buffered PDU unread.)
///
/// Blocking syscalls have no such failure mode: a blocking `read` returns whenever
/// bytes are present, and a blocking channel `recv` returns whenever an item is
/// queued. No wakeup can be lost, none is spurious, and the threads never spin when
/// idle. One thread reads + dispatches inbound PDUs; the other drains outbound PDUs
/// and notifications and writes them.
pub fn serve_local(stream: UnixStream) -> anyhow::Result<()> {
    // Two handles onto the same socket: one for reading, one for writing. They are
    // independent directions, so concurrent read and write from two threads is fine.
    let mut read_stream = stream.try_clone().context("try_clone client socket")?;
    let write_stream = stream.try_clone().context("try_clone client socket")?;
    drop(stream);

    let (item_tx, item_rx) = smol::channel::unbounded::<Item>();

    let pdu_sender = PduSender::new({
        let item_tx = item_tx.clone();
        move |pdu| {
            item_tx
                .try_send(Item::WritePdu(pdu))
                .map_err(|e| anyhow::anyhow!("{:?}", e))
        }
    });

    let handler = Arc::new(Mutex::new(SessionHandler::new(pdu_sender)));

    {
        let item_tx = item_tx.clone();
        Mux::get().subscribe(move |n| item_tx.try_send(Item::Notif(n)).is_ok());
    }

    let writer = std::thread::Builder::new()
        .name("mux-client-writer".to_string())
        .spawn({
            let handler = Arc::clone(&handler);
            move || run_writer(handler, item_rx, write_stream)
        })
        .context("spawning client writer thread")?;

    // Reader loop: block until a full PDU is available, then dispatch it. Dispatch
    // mirrors the async path: `process_one` parses and hands the real work to the
    // main thread via `spawn_into_main_thread`, sending any response back through
    // the PduSender (and thus to the writer thread).
    let result = loop {
        match Pdu::decode(&mut read_stream) {
            Ok(decoded) => {
                handler.lock().unwrap().process_one(decoded);
            }
            Err(err) => {
                let err = anyhow::Error::from(err);
                let disconnected = err
                    .root_cause()
                    .downcast_ref::<std::io::Error>()
                    .map(|e| {
                        matches!(
                            e.kind(),
                            std::io::ErrorKind::UnexpectedEof
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::BrokenPipe
                        )
                    })
                    .unwrap_or(false);
                break if disconnected { Ok(()) } else { Err(err) };
            }
        }
    };

    // Tell the writer to stop and make sure it isn't blocked on the socket.
    let _ = item_tx.try_send(Item::Shutdown);
    let _ = read_stream.shutdown(Shutdown::Both);
    let _ = writer.join();
    result
}

/// Drains outbound items for `serve_local` with a blocking channel recv and writes
/// them with blocking socket writes.
fn run_writer(
    handler: Arc<Mutex<SessionHandler>>,
    item_rx: smol::channel::Receiver<Item>,
    mut stream: std::os::unix::net::UnixStream,
) {
    while let Ok(item) = item_rx.recv_blocking() {
        let to_send = match item {
            Item::Shutdown => break,
            // Not produced on this path.
            Item::Readable => None,
            Item::WritePdu(decoded) => Some((decoded.pdu, decoded.serial)),
            Item::Notif(n) => notif_to_pdu(&handler, n).map(|pdu| (pdu, 0)),
        };
        if let Some((pdu, serial)) = to_send {
            if let Err(err) = pdu.encode(&mut stream, serial) {
                log::trace!("serve_local writer encode error: {:#}", err);
                break;
            }
            if let Err(err) = std::io::Write::flush(&mut stream) {
                log::trace!("serve_local writer flush error: {:#}", err);
                break;
            }
        }
    }
    // Unblock the reader half so the connection tears down cleanly.
    let _ = stream.shutdown(Shutdown::Both);
}

/// Maps a [`MuxNotification`] to the PDU (if any) that should be sent to the client,
/// performing any required side effects. This mirrors the per-notification handling
/// in `process_async`; keep the two in sync.
fn notif_to_pdu(handler: &Arc<Mutex<SessionHandler>>, n: MuxNotification) -> Option<Pdu> {
    match n {
        MuxNotification::PaneOutput(pane_id) => {
            handler.lock().unwrap().schedule_pane_push(pane_id);
            None
        }
        MuxNotification::Alert { pane_id, alert } => {
            let mut handler = handler.lock().unwrap();
            {
                let per_pane = handler.per_pane(pane_id);
                let mut per_pane = per_pane.lock().unwrap();
                per_pane.notifications.push(alert);
            }
            handler.schedule_pane_push(pane_id);
            None
        }
        MuxNotification::PaneRemoved(pane_id) => {
            Some(Pdu::PaneRemoved(codec::PaneRemoved { pane_id }))
        }
        MuxNotification::AssignClipboard {
            pane_id,
            selection,
            clipboard,
        } => Some(Pdu::SetClipboard(codec::SetClipboard {
            pane_id,
            clipboard,
            selection,
        })),
        MuxNotification::TabAddedToWindow { tab_id, window_id } => {
            Some(Pdu::TabAddedToWindow(codec::TabAddedToWindow {
                tab_id,
                window_id,
            }))
        }
        MuxNotification::WindowWorkspaceChanged(window_id) => {
            let workspace = Mux::get()
                .get_window(window_id)
                .map(|w| w.get_workspace().to_string());
            workspace.map(|workspace| {
                Pdu::WindowWorkspaceChanged(codec::WindowWorkspaceChanged {
                    window_id,
                    workspace,
                })
            })
        }
        MuxNotification::PaneFocused(pane_id) => {
            Some(Pdu::PaneFocused(codec::PaneFocused { pane_id }))
        }
        MuxNotification::TabResized(tab_id) => Some(Pdu::TabResized(codec::TabResized { tab_id })),
        MuxNotification::TabTitleChanged { tab_id, title } => {
            Some(Pdu::TabTitleChanged(codec::TabTitleChanged { tab_id, title }))
        }
        MuxNotification::WindowTitleChanged { window_id, title } => {
            Some(Pdu::WindowTitleChanged(codec::WindowTitleChanged {
                window_id,
                title,
            }))
        }
        MuxNotification::WorkspaceRenamed {
            old_workspace,
            new_workspace,
        } => Some(Pdu::RenameWorkspace(codec::RenameWorkspace {
            old_workspace,
            new_workspace,
        })),
        MuxNotification::PaneAdded(_)
        | MuxNotification::SaveToDownloads { .. }
        | MuxNotification::WindowRemoved(_)
        | MuxNotification::WindowCreated(_)
        | MuxNotification::WindowInvalidated(_)
        | MuxNotification::ActiveWorkspaceChanged(_)
        | MuxNotification::Empty => None,
    }
}
