//! Covers for the source browser's rows.
//!
//! An imported track's art is a file on disk, which `tracks::album_art` hands
//! to gpui's image cache by path. A DJ library's row has no such file — its
//! cover is a blob in Engine DJ's database, a thumbnail under Rekordbox's
//! `share` folder, or only the picture inside the audio's tags — so the bytes
//! come from [`crate::Library::source_artwork`] and are decoded here.
//!
//! The rules that keep a big library scrolling:
//!
//! - **Only what is on screen is asked for.** The list asks for a row's cover
//!   when it lays that row out, and `uniform_list` lays out the viewport.
//! - **Newest first, and a short memory.** Requests are served last-in first
//!   out, and the oldest unserved ones are forgotten past [`WANTED_CAP`], so
//!   after a fling the rows that are now visible are served next rather than
//!   the hundreds the list passed on the way.
//! - **Nothing heavy on the main thread.** Bytes are read on the library's
//!   runtime and decoded and shrunk to [`THUMB_PX`] on the background
//!   executor; a full-size cover never becomes a texture.
//! - **Bounded.** At most [`READY_CAP`] thumbnails are kept, and an evicted
//!   one is dropped from the window's atlas, not only from this map.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{Context, RenderImage};
use luma_lib::dispatch::ArtworkRef;

use crate::{Luma, SourceTrack};

/// The edge a thumbnail is decoded to: the row's art square at 2x, which is
/// what it is drawn at on a retina display.
pub(crate) const THUMB_PX: u32 = (super::ART_SIZE * 2.0) as u32;
/// Decoded covers kept. 512 × 64² × 4 bytes is 8 MB, and it covers several
/// screens of scrolling back.
const READY_CAP: usize = 512;
/// Unserved requests remembered — a few screens' worth of rows.
const WANTED_CAP: usize = 48;
/// Covers read and decoded at once.
const IN_FLIGHT: usize = 3;

/// A cover's identity: where its library keeps it, and the file it falls
/// back to. Two rows naming the same pair share one decode.
type ArtKey = (Option<ArtworkRef>, Option<String>);

enum Slot {
    /// Asked for, not yet started.
    Wanted,
    Loading,
    Ready(Arc<RenderImage>),
    /// Looked for and there is none (or it would not decode): the
    /// placeholder for good, and never asked for again.
    Missing,
}

#[derive(Default)]
struct Inner {
    slots: HashMap<ArtKey, Slot>,
    /// Ready keys, oldest first, for eviction.
    ready: VecDeque<ArtKey>,
    /// Unserved requests, newest last.
    wanted: Vec<(ArtKey, SourceTrack)>,
    in_flight: usize,
}

/// Shared between the dialog's state and the list's render closure, which
/// outlives any borrow of the dialog — hence the `Rc`.
#[derive(Clone, Default)]
pub(crate) struct ArtCache(Rc<RefCell<Inner>>);

/// What [`ArtCache::lookup`] found.
pub(crate) struct Lookup {
    pub(crate) image: Option<Arc<RenderImage>>,
    /// This call queued a request, so somebody has to pump.
    pub(crate) asked: bool,
}

fn key(track: &SourceTrack) -> ArtKey {
    (track.art.clone(), track.file_path.clone())
}

impl ArtCache {
    /// `track`'s cover if it has been decoded, asking for it if it has not.
    pub(crate) fn lookup(&self, track: &SourceTrack) -> Lookup {
        let key = key(track);
        let mut inner = self.0.borrow_mut();
        match inner.slots.get(&key) {
            Some(Slot::Ready(image)) => {
                return Lookup {
                    image: Some(Arc::clone(image)),
                    asked: false,
                };
            }
            Some(_) => {
                return Lookup {
                    image: None,
                    asked: false,
                };
            }
            None => {}
        }
        if key.0.is_none() && key.1.is_none() {
            inner.slots.insert(key, Slot::Missing);
            return Lookup {
                image: None,
                asked: false,
            };
        }
        inner.slots.insert(key.clone(), Slot::Wanted);
        inner.wanted.push((key, track.clone()));
        if inner.wanted.len() > WANTED_CAP {
            // Forgotten, not refused: the row asks again if it comes back.
            let (stale, _) = inner.wanted.remove(0);
            inner.slots.remove(&stale);
        }
        Lookup {
            image: None,
            asked: true,
        }
    }

    /// The next request to serve, if there is room for one.
    fn next(&self) -> Option<(ArtKey, SourceTrack)> {
        let mut inner = self.0.borrow_mut();
        if inner.in_flight >= IN_FLIGHT {
            return None;
        }
        let (key, track) = inner.wanted.pop()?;
        inner.slots.insert(key.clone(), Slot::Loading);
        inner.in_flight += 1;
        Some((key, track))
    }

    /// Record a served request; returns the thumbnails it evicted, which the
    /// caller must drop from the atlas.
    fn finish(&self, key: ArtKey, image: Option<Arc<RenderImage>>) -> Vec<Arc<RenderImage>> {
        let mut inner = self.0.borrow_mut();
        inner.in_flight -= 1;
        let Some(image) = image else {
            inner.slots.insert(key, Slot::Missing);
            return Vec::new();
        };
        inner.slots.insert(key.clone(), Slot::Ready(image));
        inner.ready.push_back(key);
        let mut evicted = Vec::new();
        while inner.ready.len() > READY_CAP {
            let Some(old) = inner.ready.pop_front() else {
                break;
            };
            if let Some(Slot::Ready(image)) = inner.slots.remove(&old) {
                evicted.push(image);
            }
        }
        evicted
    }
}

impl Luma {
    /// Serve queued cover requests up to [`IN_FLIGHT`] at once. Each one that
    /// finishes pumps again, so the queue drains without anyone polling it.
    pub(crate) fn pump_source_art(&mut self, cx: &mut Context<Self>) {
        while let Some((key, track)) = self.source_art.next() {
            let bytes = self.library.source_artwork(&track);
            let cache = self.source_art.clone();
            cx.spawn(async move |this, cx| {
                let image = match bytes.await {
                    Ok(Some(bytes)) => {
                        cx.background_executor()
                            .spawn(async move { thumbnail(&bytes) })
                            .await
                    }
                    // A cover that cannot be read shows the placeholder, which
                    // is what a row without one shows anyway.
                    Ok(None) | Err(_) => None,
                };
                this.update(cx, |this, cx| {
                    for evicted in cache.finish(key, image) {
                        cx.drop_image(evicted, None);
                    }
                    this.pump_source_art(cx);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Decode `bytes` and shrink them to a [`THUMB_PX`] square, cropping to fill
/// it so a non-square cover does not letterbox inside the square.
fn thumbnail(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(bytes).ok()?;
    let thumb = decoded
        .resize_to_fill(THUMB_PX, THUMB_PX, image::imageops::FilterType::Triangle)
        .into_rgba8();
    let (width, height) = thumb.dimensions();
    // `RenderImage` reads its buffer as BGRA.
    let mut raw = thumb.into_raw();
    for pixel in raw.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(width, height, raw)?;
    Some(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str) -> SourceTrack {
        SourceTrack {
            id: id.into(),
            file_path: Some(format!("/music/{id}.mp3")),
            filename: None,
            title: None,
            artist: None,
            album: None,
            bpm: None,
            duration_seconds: None,
            art: None,
        }
    }

    #[test]
    fn a_row_is_asked_for_once_and_served_newest_first() {
        let cache = ArtCache::default();
        assert!(cache.lookup(&track("a")).asked);
        assert!(cache.lookup(&track("b")).asked);
        // Laid out again before it is served: not a second request.
        assert!(!cache.lookup(&track("a")).asked);
        let (_, first) = cache.next().unwrap();
        assert_eq!(first.id, "b");
    }

    #[test]
    fn a_fling_forgets_the_rows_it_passed() {
        let cache = ArtCache::default();
        for index in 0..WANTED_CAP + 10 {
            cache.lookup(&track(&format!("row-{index}")));
        }
        // The first rows passed are no longer owed a decode, so they ask
        // again if they come back on screen.
        assert!(cache.lookup(&track("row-0")).asked);
    }

    #[test]
    fn a_row_with_nothing_to_read_is_never_asked_for() {
        let cache = ArtCache::default();
        let mut bare = track("bare");
        bare.file_path = None;
        assert!(!cache.lookup(&bare).asked);
        assert!(cache.next().is_none());
    }
}
