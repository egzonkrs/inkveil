//! Pictures in notes: cleaning them up when they come in, and turning stored bytes back into
//! something GPUI can draw.
//!
//! Every picture is decoded and encoded again before it is stored. That drops everything except
//! the pixels (GPS position, camera, dates and other metadata), shrinks very large pictures, and
//! means only well-formed PNG or JPEG data is ever kept. Decoding runs with size limits, so a
//! hostile file cannot exhaust memory.

use std::{collections::HashMap, io::Cursor, sync::Arc};

use gpui::{App, Global, RenderImage, Window};
use image::{DynamicImage, ImageFormat, ImageReader, Limits, imageops::FilterType};
use inkveil_store::BlobId;
use smallvec::smallvec;

/// Pictures are shrunk so their longer side is at most this many pixels.
pub const MAX_SIDE: u32 = 2560;
/// Files larger than this are refused before decoding.
pub const MAX_INPUT_BYTES: usize = 50 * 1024 * 1024;
/// New pictures are shown at this share of the text width (and never larger than they are).
pub const DEFAULT_WIDTH_PERCENT: u8 = 30;
/// File extensions offered when inserting from disk or accepted when dropped.
pub const EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// A cleaned-up picture, ready to store.
pub struct Prepared {
    /// PNG (when the picture has transparency) or JPEG bytes.
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(inkveil_doc::ImageBlock::MAX_SIDE);
    limits.max_image_height = Some(inkveil_doc::ImageBlock::MAX_SIDE);
    limits.max_alloc = Some(768 * 1024 * 1024);
    limits
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    if bytes.len() > MAX_INPUT_BYTES {
        return Err("That picture is larger than 50 MB.".into());
    }
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("Could not read the picture: {e}"))?;
    if reader.format().is_none() {
        // Clipboard bitmaps may arrive without a recognizable header.
        reader.set_format(ImageFormat::Bmp);
    }
    reader.limits(limits());
    reader
        .decode()
        .map_err(|e| format!("Could not read the picture: {e}"))
}

/// Decodes any supported picture, shrinks it if needed and re-encodes it without metadata.
pub fn prepare(input: &[u8]) -> Result<Prepared, String> {
    let mut picture = decode(input)?;
    if picture.width().max(picture.height()) > MAX_SIDE {
        picture = picture.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle);
    }
    let (width, height) = (picture.width(), picture.height());
    let mut bytes = Vec::new();
    if picture.color().has_alpha() {
        picture
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .map_err(|e| format!("Could not save the picture: {e}"))?;
    } else {
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90)
            .encode_image(&picture.to_rgb8())
            .map_err(|e| format!("Could not save the picture: {e}"))?;
    }
    Ok(Prepared {
        bytes,
        width,
        height,
    })
}

/// Turns RGBA pixels into a GPUI image (which wants BGRA).
fn to_render_image(picture: &DynamicImage) -> Arc<RenderImage> {
    let mut pixels = picture.to_rgba8();
    for pixel in pixels.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(smallvec![image::Frame::new(pixels)]))
}

/// Decodes stored picture bytes for drawing.
pub fn decode_for_display(bytes: &[u8]) -> Result<Arc<RenderImage>, String> {
    decode(bytes).map(|p| to_render_image(&p))
}

/// A tiny, blurred copy of the picture. Drawn stretched over the window behind the zoomed
/// picture, it gives a soft, dark backdrop.
pub fn backdrop(bytes: &[u8]) -> Result<Arc<RenderImage>, String> {
    let small = decode(bytes)?.thumbnail(48, 48).blur(2.5);
    Ok(to_render_image(&small))
}

/// The file extension for stored bytes ("png" or "jpg").
pub fn extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG") {
        "png"
    } else {
        "jpg"
    }
}

/// PNG bytes and a device-independent bitmap for the clipboard.
pub fn clipboard_formats(bytes: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let picture = decode(bytes)?;
    let mut png = Vec::new();
    picture
        .to_rgba8()
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let mut bmp = Vec::new();
    picture
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut bmp), ImageFormat::Bmp)
        .map_err(|e| e.to_string())?;
    // A DIB is a BMP file without its 14-byte file header.
    let dib = bmp.get(14..).ok_or("bitmap too short")?.to_vec();
    Ok((png, dib))
}

/// Decoded pictures may take this much memory before ones that are not on screen are dropped.
const CACHE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// A picture nothing has asked for in this long is not on screen. One on screen is asked for on
/// every frame.
const CACHE_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// Decoded pictures of the open notes, by attachment id, and the blurred backdrops of pictures
/// shown large. Each picture is decoded once and reused, so opening one large again costs no
/// memory. Pictures that are not on screen are dropped once the cache is over its budget; all
/// of it is emptied when the vault locks, which also frees their graphics memory.
#[derive(Default)]
pub struct ImageCache {
    images: HashMap<BlobId, Slot>,
    backdrops: HashMap<BlobId, Arc<RenderImage>>,
}

enum Slot {
    Loading,
    /// The picture, and when it was last asked for.
    Ready(Arc<RenderImage>, std::time::Instant),
    Failed,
}

/// How much memory a decoded picture takes.
fn decoded_bytes(image: &RenderImage) -> usize {
    image.as_bytes(0).map_or(0, <[u8]>::len)
}

impl Global for ImageCache {}

/// What the editor can draw for a picture right now.
pub enum Lookup {
    Ready(Arc<RenderImage>),
    Loading,
    Failed,
}

impl ImageCache {
    /// The decoded picture, starting to load it if this is the first request.
    pub fn get(id: BlobId, cx: &mut App) -> Lookup {
        let cache = cx.default_global::<Self>();
        match cache.images.get_mut(&id) {
            Some(Slot::Ready(image, used)) => {
                *used = std::time::Instant::now();
                return Lookup::Ready(image.clone());
            }
            Some(Slot::Loading) => return Lookup::Loading,
            Some(Slot::Failed) => return Lookup::Failed,
            None => {}
        }
        let Some(storage) = crate::storage::active() else {
            return Lookup::Failed;
        };
        cache.images.insert(id, Slot::Loading);
        cx.spawn(async move |cx| {
            let bytes = storage.load_blob(id).await;
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    bytes
                        .map_err(|e| e.to_string())
                        .and_then(|b| decode_for_display(&b))
                })
                .await;
            cx.update(|cx| {
                let slot = match decoded {
                    Ok(image) => Slot::Ready(image, std::time::Instant::now()),
                    Err(error) => {
                        tracing::warn!("could not show a picture: {error}");
                        Slot::Failed
                    }
                };
                cx.default_global::<Self>().images.insert(id, slot);
                Self::trim(cx);
                cx.refresh_windows();
            });
        })
        .detach();
        Lookup::Loading
    }

    /// The decoded picture if it is already loaded (without starting a load).
    pub fn ready(id: BlobId, cx: &App) -> Option<Arc<RenderImage>> {
        match cx.try_global::<Self>()?.images.get(&id)? {
            Slot::Ready(image, _) => Some(image.clone()),
            _ => None,
        }
    }

    /// Keeps a decoded picture so it is not decoded again.
    pub fn store(id: BlobId, image: Arc<RenderImage>, cx: &mut App) {
        cx.default_global::<Self>()
            .images
            .insert(id, Slot::Ready(image, std::time::Instant::now()));
        Self::trim(cx);
    }

    /// Drops pictures that are not on screen, oldest first, while the cache is over its budget.
    /// They are decoded again when they come back into view.
    fn trim(cx: &mut App) {
        let cache = cx.default_global::<Self>();
        let mut total: usize = cache
            .images
            .values()
            .map(|slot| match slot {
                Slot::Ready(image, _) => decoded_bytes(image),
                _ => 0,
            })
            .sum();
        if total <= CACHE_BUDGET_BYTES {
            return;
        }
        let mut idle: Vec<(std::time::Instant, BlobId)> = cache
            .images
            .iter()
            .filter_map(|(id, slot)| match slot {
                Slot::Ready(_, used) if used.elapsed() >= CACHE_IDLE => Some((*used, *id)),
                _ => None,
            })
            .collect();
        idle.sort_unstable_by_key(|(used, _)| *used);
        let mut dropped = Vec::new();
        for (_, id) in idle {
            if total <= CACHE_BUDGET_BYTES {
                break;
            }
            if let Some(Slot::Ready(image, _)) = cache.images.remove(&id) {
                total = total.saturating_sub(decoded_bytes(&image));
                dropped.push(image);
            }
        }
        // The window keeps its own copy for drawing until told otherwise.
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| {
                for image in &dropped {
                    let _ = window.drop_image(image.clone());
                }
            });
        }
    }

    pub fn backdrop(id: BlobId, cx: &App) -> Option<Arc<RenderImage>> {
        cx.try_global::<Self>()?.backdrops.get(&id).cloned()
    }

    pub fn store_backdrop(id: BlobId, image: Arc<RenderImage>, cx: &mut App) {
        cx.default_global::<Self>().backdrops.insert(id, image);
    }

    /// Forgets every decoded picture (when the vault locks) and frees the graphics memory the
    /// window holds for them.
    pub fn clear(window: &mut Window, cx: &mut App) {
        let old = std::mem::take(cx.default_global::<Self>());
        let images = old
            .images
            .into_values()
            .filter_map(|slot| match slot {
                Slot::Ready(image, _) => Some(image),
                _ => None,
            })
            .chain(old.backdrops.into_values());
        for image in images {
            let _ = window.drop_image(image);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: bool) -> Vec<u8> {
        let mut bytes = Vec::new();
        let picture = if alpha {
            DynamicImage::ImageRgba8(image::RgbaImage::new(width, height))
        } else {
            DynamicImage::ImageRgb8(image::RgbImage::new(width, height))
        };
        picture
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn large_pictures_are_shrunk_and_reencoded() {
        let prepared = prepare(&png(4000, 1000, false)).unwrap();
        assert_eq!((prepared.width, prepared.height), (MAX_SIDE, 640));
        assert_eq!(extension(&prepared.bytes), "jpg");
        let transparent = prepare(&png(10, 10, true)).unwrap();
        assert_eq!(extension(&transparent.bytes), "png");
    }

    #[test]
    fn garbage_is_refused() {
        assert!(prepare(b"definitely not a picture").is_err());
    }

    #[test]
    fn clipboard_bitmap_has_an_info_header() {
        let (_, dib) = clipboard_formats(&png(3, 2, false)).unwrap();
        // BITMAPINFOHEADER starts with its own size, 40.
        assert_eq!(u32::from_le_bytes(dib[..4].try_into().unwrap()), 40);
    }
}
