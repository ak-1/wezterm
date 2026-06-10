use smithay_client_toolkit::compositor::SurfaceData;
use smithay_client_toolkit::data_device_manager::data_device::DataDeviceHandler;
use smithay_client_toolkit::data_device_manager::data_offer::DataOfferHandler;
use smithay_client_toolkit::data_device_manager::data_source::DataSourceHandler;
use smithay_client_toolkit::data_device_manager::WritePipe;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::Proxy;

use crate::wayland::pointer::PointerUserData;
use crate::wayland::SurfaceUserData;

use super::copy_and_paste::write_selection_to_pipe;
use super::drag_and_drop::{DragAndDropSession, WindowAndPipe};
use super::state::WaylandState;

pub(super) const TEXT_MIME_TYPE: &str = "text/plain;charset=utf-8";
pub(super) const URI_MIME_TYPE: &str = "text/uri-list";
pub(super) const IMAGE_PNG_MIME_TYPE: &str = "image/png";

impl DataDeviceHandler for WaylandState {
    fn enter(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &WlSurface,
    ) {
        let data = match self.data_device {
            Some(ref dv) if dv.inner() == data_device => dv.data(),
            _ => {
                log::warn!("No existing device manager for {:?}", data_device);
                return;
            }
        };

        let Some(drag_offer) = data.drag_offer() else {
            log::warn!("drag enter without a drag offer");
            return;
        };

        log::trace!("Data offer entered: {:?}", drag_offer);

        // The drag may enter one of our non-window surfaces, e.g. a CSD
        // frame subsurface (the title bar); those carry SCTK's plain
        // SurfaceData rather than our SurfaceUserData, so resolve them to
        // the window via their parent surface.
        let window_id = match SurfaceUserData::try_from_wl(&drag_offer.surface) {
            Some(surface_data) => surface_data.window_id,
            None => {
                let parent = drag_offer
                    .surface
                    .data::<SurfaceData>()
                    .and_then(|data| data.parent_surface())
                    .and_then(SurfaceUserData::try_from_wl);
                match parent {
                    Some(surface_data) => surface_data.window_id,
                    None => {
                        log::warn!("drag entered an unknown surface; ignoring the offer");
                        return;
                    }
                }
            }
        };

        drag_offer.with_mime_types(|mime_types| {
            log::trace!("Data offer mime_types: {:?}", mime_types);

            if let Some(mime) = mime_types.iter().find(|s| *s == URI_MIME_TYPE) {
                drag_offer.accept_mime_type(*self.last_serial.borrow(), Some(mime.clone()));
            }
        });

        drag_offer.set_actions(DndAction::None | DndAction::Copy, DndAction::None);

        let Some(pointer) = self.pointer.as_mut() else {
            log::warn!("drag enter with no pointer to track it");
            return;
        };
        let mut pstate = pointer
            .pointer()
            .data::<PointerUserData>()
            .unwrap()
            .state
            .lock()
            .unwrap();

        pstate.drag_and_drop_session = Some(DragAndDropSession { window_id, drag_offer });
        log::trace!("DnD: session started for window_id={}", window_id);
    }

    fn leave(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
        let Some(pointer) = self.pointer.as_mut() else {
            return;
        };
        let mut pstate = pointer
            .pointer()
            .data::<PointerUserData>()
            .unwrap()
            .state
            .lock()
            .unwrap();
        if let Some(session) = pstate.drag_and_drop_session.take() {
            session.drag_offer.destroy();
            log::trace!("DnD: session ended for window_id={}", session.window_id);
        }
    }

    fn motion(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        let offer = match self.data_device {
            Some(ref dv) if dv.inner() == data_device => dv.data().selection_offer(),
            _ => {
                return;
            }
        };
        if let Some(offer) = offer {
            if !offer.with_mime_types(|mime_types| has_accepted_mime_type(mime_types)) {
                return;
            }
            // The compositor sends the selection event once per client.
            // ref: https://github.com/wezterm/wezterm/issues/6685
            self.copy_paste_offer
                .lock()
                .unwrap()
                .confirm_selection(offer);
        }
    }

    fn drop_performed(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
        let Some(pointer) = self.pointer.as_mut() else {
            return;
        };
        let mut pstate = pointer
            .pointer()
            .data::<PointerUserData>()
            .unwrap()
            .state
            .lock()
            .unwrap();
        let Some(mut dnd_session) = pstate.drag_and_drop_session.take() else {
            log::warn!("DnD: in drop_performed but no session active");
            return;
        };
        if let Some(WindowAndPipe { window_id, read }) = dnd_session.create_pipe_for_drop() {
            std::thread::spawn(move || {
                if let Some(paths) = DragAndDropSession::read_paths_from_pipe(read) {
                    DragAndDropSession::dispatch_dropped_files(window_id, paths);
                }
            });
        }
    }
}

impl DataOfferHandler for WaylandState {
    // Ignore drag and drop events
    fn source_actions(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _offer: &mut smithay_client_toolkit::data_device_manager::data_offer::DragOffer,
        _actions: wayland_client::protocol::wl_data_device_manager::DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _offer: &mut smithay_client_toolkit::data_device_manager::data_offer::DragOffer,
        _actions: wayland_client::protocol::wl_data_device_manager::DndAction,
    ) {
    }
}

// We seem to ignore all events other than sending_request and cancelled
impl DataSourceHandler for WaylandState {
    fn accept_mime(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _source: &wayland_client::protocol::wl_data_source::WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        source: &wayland_client::protocol::wl_data_source::WlDataSource,
        mime: String,
        fd: WritePipe,
    ) {
        if mime != TEXT_MIME_TYPE {
            return;
        }

        if let Some((cp_source, data)) = &self.copy_paste_source {
            if cp_source.inner() != source {
                return;
            }
            write_selection_to_pipe(fd, data);
        }
    }

    fn cancelled(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        source: &wayland_client::protocol::wl_data_source::WlDataSource,
    ) {
        self.copy_paste_source.take();
        source.destroy();
    }

    fn dnd_dropped(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _source: &wayland_client::protocol::wl_data_source::WlDataSource,
    ) {
    }

    fn dnd_finished(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _source: &wayland_client::protocol::wl_data_source::WlDataSource,
    ) {
    }

    fn action(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _source: &wayland_client::protocol::wl_data_source::WlDataSource,
        _action: wayland_client::protocol::wl_data_device_manager::DndAction,
    ) {
    }
}

/// Returns true if the given MIME types contain at least one type that we
/// accept for clipboard selection (text or PNG image).
fn has_accepted_mime_type(mime_types: &[String]) -> bool {
    mime_types
        .iter()
        .any(|s| s == TEXT_MIME_TYPE || s == IMAGE_PNG_MIME_TYPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accept_text_mime_type() {
        let mime_types = vec!["text/plain;charset=utf-8".to_string()];
        assert!(has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_accept_image_png_mime_type() {
        let mime_types = vec!["image/png".to_string()];
        assert!(has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_accept_both_text_and_image() {
        let mime_types = vec![
            "text/plain;charset=utf-8".to_string(),
            "image/png".to_string(),
        ];
        assert!(has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_reject_unsupported_mime_types() {
        let mime_types = vec!["text/html".to_string(), "application/json".to_string()];
        assert!(!has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_reject_empty_mime_types() {
        let mime_types: Vec<String> = vec![];
        assert!(!has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_accept_image_png_among_other_types() {
        let mime_types = vec![
            "text/html".to_string(),
            "image/png".to_string(),
            "application/octet-stream".to_string(),
        ];
        assert!(has_accepted_mime_type(&mime_types));
    }

    #[test]
    fn test_reject_other_image_formats() {
        let mime_types = vec![
            "image/jpeg".to_string(),
            "image/gif".to_string(),
            "image/bmp".to_string(),
        ];
        assert!(!has_accepted_mime_type(&mime_types));
    }
}
