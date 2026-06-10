use std::cell::RefCell;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::sync::atomic::AtomicUsize;

use anyhow::{bail, Context};
use wayland_client::backend::WaylandError;
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection as WConnection, EventQueue};

use crate::screen::{ScreenInfo, Screens};
use crate::spawn::SPAWN_QUEUE;
use crate::{Appearance, Connection, ConnectionOps, ScreenRect};

use super::state::WaylandState;
use super::WaylandWindowInner;

pub struct WaylandConnection {
    pub(crate) should_terminate: RefCell<bool>,
    pub(crate) next_window_id: AtomicUsize,
    pub(super) gl_connection: RefCell<Option<Rc<crate::egl::GlConnection>>>,
    pub(super) connection: WConnection,
    pub(super) event_queue: RefCell<EventQueue<WaylandState>>,
    pub(super) wayland_state: RefCell<WaylandState>,
}

impl WaylandConnection {
    pub(crate) fn create_new() -> anyhow::Result<Self> {
        let conn = WConnection::connect_to_env()?;
        let (globals, event_queue) = registry_queue_init::<WaylandState>(&conn)?;
        let qh = event_queue.handle();

        let wayland_state = WaylandState::new(&globals, &qh)?;
        let wayland_connection = WaylandConnection {
            connection: conn,
            should_terminate: RefCell::new(false),
            next_window_id: AtomicUsize::new(1),
            gl_connection: RefCell::new(None),
            event_queue: RefCell::new(event_queue),
            wayland_state: RefCell::new(wayland_state),
        };

        Ok(wayland_connection)
    }

    pub(crate) fn advise_of_appearance_change(&self, appearance: crate::Appearance) {
        for win in self.wayland_state.borrow().windows.borrow().values() {
            win.borrow_mut().appearance_changed(appearance);
        }
    }

    fn run_message_loop_impl(&self) -> anyhow::Result<()> {
        let spawn_fd = SPAWN_QUEUE.raw_fd();

        while !*self.should_terminate.borrow() {
            // Run a pending spawned function; if more remain we don't want to
            // block in poll, so we'll use a zero timeout below.
            let timeout_ms: libc::c_int = if SPAWN_QUEUE.run() { 0 } else { -1 };

            // Dispatch whatever is already queued, then prepare to read more.
            // dispatch_pending must run unconditionally every iteration: events
            // read into the queue on the previous iteration (or demuxed into it
            // by an unrelated socket read, e.g. Mesa's EGL swap reading the
            // wayland fd to throttle) are only delivered to their handlers here.
            // prepare_read() does *not* report those as pending, so gating the
            // dispatch on it would silently drop events. The wayland-backend
            // contract also requires the ReadEventsGuard to be created *before*
            // we poll the socket, which we satisfy by holding it across poll.
            let read_guard = loop {
                {
                    let mut event_q = self.event_queue.borrow_mut();
                    let mut wayland_state = self.wayland_state.borrow_mut();
                    event_q
                        .dispatch_pending(&mut wayland_state)
                        .context("error during event_q.dispatch_pending")?;
                }
                match self.event_queue.borrow().prepare_read() {
                    Some(guard) => break guard,
                    // Events arrived between the dispatch above and now; loop
                    // to dispatch them before we sleep.
                    None => continue,
                }
            };

            let wl_fd = read_guard.connection_fd().as_raw_fd();

            // Flush our pending requests. WouldBlock here means the socket's
            // send buffer is full (the compositor is slow to read us); the
            // unsent requests stay buffered in the backend, so it is not
            // fatal. Ask poll below to additionally wake us when the socket
            // becomes writable, and retry the flush on the next iteration.
            let mut flush_pending = false;
            match self.event_queue.borrow().flush() {
                Ok(()) => {}
                Err(WaylandError::Io(ref err))
                    if err.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    flush_pending = true;
                }
                Err(err) => {
                    return Err(err).context("error flushing wayland event queue");
                }
            }

            // Use a level-triggered libc::poll rather than mio's
            // edge-triggered epoll: level-triggering re-reports a fd as ready
            // for as long as it still holds unread data, so a partially
            // drained socket (or an edge consumed by another reader) can never
            // strand us asleep with a frame-callback reply -- or any other
            // event -- still waiting to be read. That edge-triggered stall is
            // what made the window freeze until an unrelated keypress.
            let mut pfd = [
                libc::pollfd {
                    fd: wl_fd,
                    events: libc::POLLIN | if flush_pending { libc::POLLOUT } else { 0 },
                    revents: 0,
                },
                libc::pollfd {
                    fd: spawn_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];

            let res = unsafe { libc::poll(pfd.as_mut_ptr(), pfd.len() as _, timeout_ms) };
            if res < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    // Dropping read_guard cancels the prepared read.
                    continue;
                }
                bail!("polling for events: {:#}", err);
            }

            if pfd[0].revents & libc::POLLIN != 0 {
                // Read what's available into the queue; it is dispatched at the
                // top of the next iteration.
                if let Err(err) = read_guard.read() {
                    match err {
                        WaylandError::Io(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        WaylandError::Protocol(perr) => return Err(perr.into()),
                        other => log::trace!("error reading wayland events: {:#}", other),
                    }
                }
            } else {
                // Nothing to read from the socket; cancel the prepared read so
                // the next iteration is free to dispatch.
                drop(read_guard);
            }
        }

        Ok(())
    }

    pub(crate) fn next_window_id(&self) -> usize {
        self.next_window_id
            .fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn window_by_id(&self, window_id: usize) -> Option<Rc<RefCell<WaylandWindowInner>>> {
        self.wayland_state.borrow().window_by_id(window_id)
    }

    pub(crate) fn with_window_inner<
        R,
        F: FnOnce(&mut WaylandWindowInner) -> anyhow::Result<R> + Send + 'static,
    >(
        window: usize,
        f: F,
    ) -> promise::Future<R>
    where
        R: Send + 'static,
    {
        let mut prom = promise::Promise::new();
        let future = prom.get_future().unwrap();

        promise::spawn::spawn_into_main_thread(async move {
            if let Some(handle) = Connection::get().unwrap().wayland().window_by_id(window) {
                let mut inner = handle.borrow_mut();
                prom.result(f(&mut inner));
            }
        })
        .detach();

        future
    }
}

impl ConnectionOps for WaylandConnection {
    fn name(&self) -> String {
        "Wayland".to_string()
    }

    fn terminate_message_loop(&self) {
        log::trace!("Terminating Message Loop");
        *self.should_terminate.borrow_mut() = true;
    }

    fn run_message_loop(&self) -> anyhow::Result<()> {
        let res = self.run_message_loop_impl();
        // Ensure that we drop these eagerly, to avoid
        // noisy errors wrt. global destructors unwinding
        // in unexpected places
        self.wayland_state.borrow().windows.borrow_mut().clear();
        res
    }

    fn get_appearance(&self) -> Appearance {
        match promise::spawn::block_on(crate::os::xdg_desktop_portal::get_appearance()) {
            Ok(Some(appearance)) => return appearance,
            Ok(None) => {}
            Err(err) => {
                log::warn!("Unable to resolve appearance using xdg-desktop-portal: {err:#}");
            }
        }
        // fallback
        Appearance::Light
    }

    fn screens(&self) -> anyhow::Result<crate::screen::Screens> {
        log::trace!("Getting screens for wayland connection");

        if let Some(output_manager) = &self.wayland_state.borrow().output_manager {
            if let Some(screens) = output_manager.screens() {
                return Ok(screens);
            }
        }

        let mut by_name = HashMap::new();
        let mut virtual_rect: ScreenRect = euclid::rect(0, 0, 0, 0);
        let config = config::configuration();

        let output_state = &self.wayland_state.borrow().output;

        for output in output_state.outputs() {
            let info = match output_state.info(&output) {
                Some(i) => i,
                None => continue,
            };
            let name = match info.name {
                Some(n) => n.clone(),
                None => format!("{} {}", info.model, info.make),
            };

            let (width, height) = info
                .modes
                .iter()
                .find(|mode| mode.current)
                .map(|mode| mode.dimensions)
                .unwrap_or((info.physical_size.0, info.physical_size.1));

            let rect = euclid::rect(
                info.location.0 as isize,
                info.location.1 as isize,
                width as isize,
                height as isize,
            );

            let scale = info.scale_factor as f64;

            // FIXME: teach this how to resolve dpi_by_screen once
            // dispatch_pending_event knows how to do the same
            let effective_dpi = Some(config.dpi.unwrap_or(scale * crate::DEFAULT_DPI));

            virtual_rect = virtual_rect.union(&rect);
            by_name.insert(
                name.clone(),
                ScreenInfo {
                    name,
                    rect,
                    scale,
                    max_fps: None,
                    effective_dpi,
                },
            );
        }

        // // The main screen is the one either at the origin of
        // // the virtual area, or if that doesn't exist for some weird
        // // reason, the screen closest to the origin.
        let main = by_name
            .values()
            .min_by_key(|screen| {
                screen
                    .rect
                    .origin
                    .to_f32()
                    .distance_to(euclid::Point2D::origin())
                    .abs() as isize
            })
            .ok_or_else(|| anyhow::anyhow!("no screens were found"))?
            .clone();

        // We don't yet know how to determine the active screen,
        // so assume the main screen.
        let active = main.clone();

        Ok(Screens {
            main,
            active,
            by_name,
            virtual_rect,
        })
    }
}
