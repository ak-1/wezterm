use std::sync::{Arc, Mutex};
use std::time::Duration;

use smithay_client_toolkit::compositor::SurfaceData;
use smithay_client_toolkit::reexports::csd_frame::{DecorationsFrame, FrameClick};
use smithay_client_toolkit::seat::pointer::{
    CursorIcon, PointerData, PointerDataExt, PointerEvent, PointerEventKind, PointerHandler,
};
use wayland_client::protocol::wl_pointer::{ButtonState, WlPointer};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Proxy, QueueHandle};
use wezterm_input_types::MousePress;

use crate::wayland::SurfaceUserData;

use super::copy_and_paste::CopyAndPaste;
use super::drag_and_drop::DragAndDrop;
use super::state::WaylandState;
use super::WaylandConnection;

impl PointerHandler for WaylandState {
    fn pointer_frame(
        &mut self,
        conn: &Connection,
        _qh: &QueueHandle<Self>,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        for evt in events {
            if let Some(serial) = event_serial(&evt) {
                *self.last_serial.borrow_mut() = serial;
            }
            // Route by the surface each event names. Pointer focus and
            // keyboard focus are independent: active_surface_id tracks the
            // keyboard focus (it routes incoming selection offers and IME
            // state), and routing pointer events by it delivered them to
            // the keyboard-focused window whenever the pointer was hovering
            // a different one -- stuck buttons in one window, selections in
            // the other.
            if let Some(pending) = self.surface_to_pending.get(&evt.surface.id()) {
                let mut pending = pending.lock().unwrap();
                if pending.queue(evt) {
                    WaylandConnection::with_window_inner(pending.window_id, move |inner| {
                        inner.dispatch_pending_mouse();
                        Ok(())
                    });
                }
            }
        }
        self.pointer_window_frame(conn, pointer, events);
    }
}

pub(super) struct PointerUserData {
    pub(super) pdata: PointerData,
    pub(super) state: Mutex<PointerState>,
}

impl PointerUserData {
    pub(super) fn new(seat: WlSeat) -> Self {
        Self {
            pdata: PointerData::new(seat),
            state: Default::default(),
        }
    }
}

#[derive(Default)]
pub(super) struct PointerState {
    pub(super) drag_and_drop: DragAndDrop,
}

impl PointerDataExt for PointerUserData {
    fn pointer_data(&self) -> &PointerData {
        &self.pdata
    }
}

#[derive(Clone, Debug)]
pub struct PendingMouse {
    window_id: usize,
    pub(super) copy_and_paste: Arc<Mutex<CopyAndPaste>>,
    surface_coords: Option<(f64, f64)>,
    button: Vec<(MousePress, ButtonState)>,
    scroll: Option<(f64, f64)>,
    in_window: bool,
}

impl PendingMouse {
    pub(super) fn create(
        window_id: usize,
        copy_and_paste: &Arc<Mutex<CopyAndPaste>>,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            window_id,
            copy_and_paste: Arc::clone(copy_and_paste),
            button: vec![],
            scroll: None,
            surface_coords: None,
            in_window: false,
        }))
    }

    pub(super) fn queue(&mut self, evt: &PointerEvent) -> bool {
        match evt.kind {
            PointerEventKind::Enter { .. } => {
                self.in_window = true;
                false
            }
            PointerEventKind::Leave { .. } => {
                let changed = self.in_window;
                self.surface_coords = None;
                self.in_window = false;
                changed
            }
            PointerEventKind::Motion { .. } => {
                // Receiving motion means the pointer is over our surface, so
                // re-assert that we're in the window. After an interactive move
                // (xdg_toplevel.move) the compositor sends a `leave` when the
                // grab starts but does not always send a matching `enter` when
                // it ends (notably mutter); without this the window stays stuck
                // in the "left" state, which suppresses cursor updates (see
                // set_cursor) and makes dispatch_pending_mouse emit a spurious
                // MouseLeave after every motion.
                let changed = self.surface_coords.is_none() || !self.in_window;
                self.in_window = true;
                self.surface_coords.replace(evt.position);
                changed
            }
            PointerEventKind::Press { button, .. } | PointerEventKind::Release { button, .. } => {
                fn linux_button(b: u32) -> Option<MousePress> {
                    // See BTN_LEFT and friends in <linux/input-event-codes.h>
                    match b {
                        0x110 => Some(MousePress::Left),
                        0x111 => Some(MousePress::Right),
                        0x112 => Some(MousePress::Middle),
                        _ => None,
                    }
                }
                let button = match linux_button(button) {
                    Some(button) => button,
                    None => return false,
                };
                let changed = self.button.is_empty();
                let button_state = match evt.kind {
                    PointerEventKind::Press { .. } => ButtonState::Pressed,
                    PointerEventKind::Release { .. } => ButtonState::Released,
                    _ => unreachable!(),
                };
                self.button.push((button, button_state));
                changed
            }
            PointerEventKind::Axis {
                horizontal,
                vertical,
                ..
            } => {
                let changed = self.scroll.is_none();
                let (x, y) = self.scroll.take().unwrap_or((0., 0.));
                self.scroll
                    .replace((x + horizontal.absolute, y + vertical.absolute));
                changed
            }
        }
    }

    pub(super) fn next_button(pending: &Arc<Mutex<Self>>) -> Option<(MousePress, ButtonState)> {
        let mut pending = pending.lock().unwrap();
        if pending.button.is_empty() {
            None
        } else {
            Some(pending.button.remove(0))
        }
    }

    pub(super) fn coords(pending: &Arc<Mutex<Self>>) -> Option<(f64, f64)> {
        pending.lock().unwrap().surface_coords.take()
    }

    pub(super) fn scroll(pending: &Arc<Mutex<Self>>) -> Option<(f64, f64)> {
        pending.lock().unwrap().scroll.take()
    }

    pub(super) fn in_window(pending: &Arc<Mutex<Self>>) -> bool {
        pending.lock().unwrap().in_window
    }
}

fn event_serial(event: &PointerEvent) -> Option<u32> {
    Some(match event.kind {
        PointerEventKind::Enter { serial, .. } => serial,
        PointerEventKind::Leave { serial, .. } => serial,
        PointerEventKind::Press { serial, .. } => serial,
        PointerEventKind::Release { serial, .. } => serial,
        _ => return None,
    })
}

impl WaylandState {
    fn pointer_window_frame(
        &mut self,
        conn: &Connection,
        pointer: &WlPointer,
        events: &[PointerEvent],
    ) {
        let windows = self.windows.borrow();

        // The cursor shape the frame wants for the region under the pointer, if
        // the pointer is currently over a (visible) frame surface. We apply it
        // after the loop, once the per-window borrow has been released.
        let mut frame_cursor: Option<Option<CursorIcon>> = None;

        for evt in events {
            // Frame events arrive on the decoration subsurfaces; resolve
            // those to the window that owns them via their parent surface.
            // Events on a window's main surface (which has no parent) are
            // not frame events.
            let parent_surface = match evt.surface.data::<SurfaceData>() {
                Some(data) => match data.parent_surface() {
                    Some(sd) => sd,
                    None => continue,
                },
                None => continue,
            };

            let Some(surface_data) = SurfaceUserData::try_from_wl(parent_surface) else {
                continue;
            };
            let Some(window) = windows.get(&surface_data.window_id) else {
                continue;
            };
            let mut inner = window.borrow_mut();
            let (x, y) = evt.position;

            match evt.kind {
                PointerEventKind::Enter { .. } => {
                    let icon = inner.window_frame.click_point_moved(
                        Duration::ZERO,
                        &evt.surface.id(),
                        x,
                        y,
                    );
                    if !inner.window_frame.is_hidden() {
                        frame_cursor = Some(icon);
                    }
                }
                PointerEventKind::Leave { .. } => {
                    inner.window_frame.click_point_left();
                }
                PointerEventKind::Motion { .. } => {
                    let icon = inner.window_frame.click_point_moved(
                        Duration::ZERO,
                        &evt.surface.id(),
                        x,
                        y,
                    );
                    if !inner.window_frame.is_hidden() {
                        frame_cursor = Some(icon);
                    }
                }
                PointerEventKind::Press { button, serial, .. }
                | PointerEventKind::Release { button, serial, .. } => {
                    let pressed = matches!(evt.kind, PointerEventKind::Press { .. });
                    let click = match button {
                        0x110 => FrameClick::Normal,
                        0x111 => FrameClick::Alternate,
                        _ => continue,
                    };
                    if let Some(action) =
                        inner.window_frame.on_click(Duration::ZERO, click, pressed)
                    {
                        inner.frame_action(pointer, serial, action);
                    }
                }
                _ => {}
            }
        }

        drop(windows);

        // SCTK's frame tells us which cursor to show for the region under the
        // pointer (resize arrows on the borders/corners, the default arrow over
        // the title bar). It does *not* apply it for us; without this the cursor
        // would keep whatever shape the terminal area last requested (typically
        // the text I-beam) while hovering the decorations.
        if let Some(icon) = frame_cursor {
            if let Some(themed_pointer) = &self.pointer {
                let icon = icon.unwrap_or(CursorIcon::Default);
                if let Err(err) = themed_pointer.set_cursor(conn, icon) {
                    log::error!("set_cursor (frame): {}", err);
                }
            }
        }
    }
}
