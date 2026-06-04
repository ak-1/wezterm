use crate::macos::{nsstring, nsstring_to_str};
use cocoa::appkit::{
    NSFilenamesPboardType, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeTIFF,
    NSStringPboardType,
};
use cocoa::base::*;
use cocoa::foundation::NSArray;
use objc::{msg_send, sel, sel_impl};

pub struct Clipboard {
    pasteboard: id,
}

impl Clipboard {
    pub fn new() -> Self {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard(nil) };
        if pasteboard.is_null() {
            panic!("NSPasteboard::generalPasteboard returned null");
        }
        Clipboard { pasteboard }
    }

    pub fn read(&self) -> anyhow::Result<String> {
        unsafe {
            let plist = self.pasteboard.propertyListForType(NSFilenamesPboardType);
            if !plist.is_null() {
                let mut filenames = vec![];
                for i in 0..plist.count() {
                    filenames.push(
                        shlex::try_quote(nsstring_to_str(plist.objectAtIndex(i)))
                            .unwrap_or_else(|_| "".into()),
                    );
                }
                return Ok(filenames.join(" "));
            }
            let s = self.pasteboard.stringForType(NSStringPboardType);
            if !s.is_null() {
                let str = nsstring_to_str(s);
                return Ok(str.to_string());
            }
        }
        anyhow::bail!("pasteboard read returned empty");
    }

    /// Read an image off the pasteboard as PNG-encoded bytes.
    /// Prefers a PNG payload (no conversion needed); otherwise falls back to
    /// TIFF (what screen captures place on the pasteboard) and transcodes it.
    pub fn read_image(&self) -> anyhow::Result<Vec<u8>> {
        unsafe {
            let png = self.pasteboard.dataForType(NSPasteboardTypePNG);
            if !png.is_null() {
                let bytes = nsdata_to_vec(png);
                if !bytes.is_empty() {
                    return Ok(bytes);
                }
            }

            let tiff = self.pasteboard.dataForType(NSPasteboardTypeTIFF);
            if !tiff.is_null() {
                let bytes = nsdata_to_vec(tiff);
                if !bytes.is_empty() {
                    return tiff_to_png(&bytes);
                }
            }
        }
        anyhow::bail!("clipboard does not contain an image");
    }

    pub fn write(&mut self, data: String) -> anyhow::Result<()> {
        unsafe {
            self.pasteboard.clearContents();
            let success: BOOL = self
                .pasteboard
                .writeObjects(NSArray::arrayWithObject(nil, *nsstring(&data)));
            anyhow::ensure!(success == YES, "pasteboard write returned false");
            Ok(())
        }
    }
}

/// Copy the contents of an `NSData` object into an owned `Vec`.
unsafe fn nsdata_to_vec(data: id) -> Vec<u8> {
    let len: usize = msg_send![data, length];
    let bytes: *const u8 = msg_send![data, bytes];
    if bytes.is_null() || len == 0 {
        return Vec::new();
    }
    std::slice::from_raw_parts(bytes, len).to_vec()
}

/// Decode TIFF image bytes and re-encode them as PNG.
fn tiff_to_png(tiff: &[u8]) -> anyhow::Result<Vec<u8>> {
    use image::ImageFormat;
    use std::io::Cursor;

    let image = image::load_from_memory_with_format(tiff, ImageFormat::Tiff)
        .map_err(|e| anyhow::anyhow!("decoding clipboard TIFF: {}", e))?;

    let mut png = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("encoding clipboard image as PNG: {}", e))?;
    Ok(png)
}
