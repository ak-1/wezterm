use crate::termwindow::TermWindowNotif;
use crate::TermWindow;
use config::keyassignment::{ClipboardCopyDestination, ClipboardPasteSource};
use mux::pane::Pane;
use mux::Mux;
use std::sync::Arc;
use window::{Clipboard, WindowOps};

impl TermWindow {
    pub fn copy_to_clipboard(&self, clipboard: ClipboardCopyDestination, text: String) {
        let clipboard = match clipboard {
            ClipboardCopyDestination::Clipboard => [Some(Clipboard::Clipboard), None],
            ClipboardCopyDestination::PrimarySelection => [Some(Clipboard::PrimarySelection), None],
            ClipboardCopyDestination::ClipboardAndPrimarySelection => [
                Some(Clipboard::Clipboard),
                Some(Clipboard::PrimarySelection),
            ],
        };
        for &c in &clipboard {
            if let Some(c) = c {
                self.window.as_ref().unwrap().set_clipboard(c, text.clone());
            }
        }
    }

    pub fn paste_from_clipboard(&mut self, pane: &Arc<dyn Pane>, clipboard: ClipboardPasteSource) {
        let pane_id = pane.pane_id();
        log::trace!(
            "paste_from_clipboard in pane {} {:?}",
            pane.pane_id(),
            clipboard
        );
        let window = self.window.as_ref().unwrap().clone();
        let clipboard = match clipboard {
            ClipboardPasteSource::Clipboard => Clipboard::Clipboard,
            ClipboardPasteSource::PrimarySelection => Clipboard::PrimarySelection,
        };
        let future = window.get_clipboard(clipboard);
        promise::spawn::spawn(async move {
            if let Ok(clip) = future.await {
                window.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                    if let Some(pane) = myself
                        .pane_state(pane_id)
                        .overlay
                        .as_ref()
                        .map(|overlay| overlay.pane.clone())
                        .or_else(|| {
                            let mux = Mux::get();
                            mux.get_pane(pane_id)
                        })
                    {
                        pane.send_paste(&clip).ok();
                    }
                })));
            }
        })
        .detach();
        self.maybe_scroll_to_bottom_for_input(&pane);
    }

    /// "Smart" paste: if the clipboard holds an image, deliver it to the pane's
    /// host (writing a temp file there and pasting its path); otherwise fall
    /// back to a normal text paste. This is what makes image paste work for
    /// remote panes, where the pane's process cannot read the local clipboard.
    pub fn paste_image_from_clipboard(
        &mut self,
        pane: &Arc<dyn Pane>,
        clipboard: ClipboardPasteSource,
    ) {
        let pane_id = pane.pane_id();
        let window = self.window.as_ref().unwrap().clone();
        let image_future = window.get_clipboard_image_data();
        let win_for_async = window.clone();
        promise::spawn::spawn(async move {
            let image_result = match image_future.await {
                Ok(data) if !data.is_empty() => Some(data),
                _ => None,
            };
            win_for_async.notify(TermWindowNotif::Apply(Box::new(move |myself| {
                let pane = match myself
                    .pane_state(pane_id)
                    .overlay
                    .as_ref()
                    .map(|overlay| overlay.pane.clone())
                    .or_else(|| {
                        let mux = Mux::get();
                        mux.get_pane(pane_id)
                    }) {
                    Some(pane) => pane,
                    None => return,
                };
                match image_result {
                    Some(data) => {
                        if let Err(err) = pane.send_image_paste(data) {
                            log::error!("paste_image_from_clipboard: {:#}", err);
                        }
                    }
                    None => {
                        // No image on the clipboard; behave like a normal paste.
                        myself.paste_from_clipboard(&pane, clipboard);
                    }
                }
            })));
        })
        .detach();
        self.maybe_scroll_to_bottom_for_input(&pane);
    }
}
