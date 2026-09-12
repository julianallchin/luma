//! CPU side of the native-haze lit-interval cache
//! (`docs/design/haze-lit-interval-cache.md`).
//!
//! The shader caches, per (pixel, shadow slot), the output of the shadow-map
//! traversal: whether the whole beam span was lit, or the list of lit
//! `t`-intervals it integrated. This module decides, once per frame and per
//! shadow slot, whether that stored output is still the answer — every input
//! to the traversal is either frame-global (camera, opaque depth) or per slot
//! (shadow projection and map contents, range, field, wash), so validity is
//! one bit per slot — and hands each resident slot a contiguous region of the
//! header pool sized to its screen rect. No per-pixel keys exist anywhere.
//!
//! Regions are first-fit allocated and survive as long as the slot's key does.
//! When the pool cannot fit a new region, everything is compacted and every
//! slot takes one miss frame; the shader never reads a region that has not
//! been written under the current key.

use crate::shadow::ShadowCacheKey;

/// Everything the traversal reads that is global to the frame, as exact bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FrameKey {
    /// `inv_view_proj` (16), camera position (3), haze target size (2),
    /// depth-buffer size (2), camera near/far planes (2).
    pub camera: [u32; 25],
    /// Identity of every opaque draw that wrote the depth buffer.
    pub depth: u64,
}

/// Everything the traversal reads that belongs to one shadow slot, as exact
/// bits, plus the slot's screen rect in cache blocks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotKey {
    /// Shadow projection and caster identity: the same key that decides
    /// whether the depth map itself is redrawn.
    pub shadow: ShadowCacheKey,
    /// Raw cone range, field cosine and wash — `beam_span` reads them and the
    /// shadow key only carries their clamped, derived forms.
    pub range: u32,
    pub cos_field: u32,
    pub wash: u32,
    /// Whether the cone scatters at all; a non-scattering cone never traverses
    /// and therefore never writes its region.
    pub scatters: bool,
    /// `[x0, y0, width, height]` in 8×4 cache blocks. Part of the key because
    /// the region's addressing depends on it.
    pub rect: [u32; 4],
}

impl SlotKey {
    fn blocks(&self) -> u32 {
        self.rect[2] * self.rect[3]
    }
}

/// Per-slot GPU record: `[base, x0 | y0 << 16, w | h << 16, mode | gen << 8]`
/// with mode 0 = uncached (traverse, never touch the pool), 1 = read,
/// 2 = write. `gen` counts the slot's write frames; payload entries carry the
/// generation they were written under so a stale entry can never be replayed.
pub(crate) type SlotEntry = [u32; 4];

/// Intervals stored per payload entry; must match `CACHE_K` in
/// `beam_transport.wgsl`.
pub(crate) const CACHE_K: u32 = 8;

pub(crate) const MODE_OFF: u32 = 0;
pub(crate) const MODE_READ: u32 = 1;
pub(crate) const MODE_WRITE: u32 = 2;

#[derive(Clone, Copy, Debug, Default)]
struct SlotState {
    key: Option<SlotKey>,
    /// `(start, length)` in header words, when the slot holds a region.
    region: Option<(u32, u32)>,
    /// Write generation, bumped on every write frame. 24 bits reach the GPU.
    gen: u32,
    /// The region holds this key's output: a write frame has run since the
    /// key was set.
    written: bool,
}

/// How many slots took each mode on the last plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct IntervalCacheStats {
    /// Slots whose stored traversal output was replayed.
    pub read: usize,
    /// Slots that traversed and stored their output.
    pub write: usize,
    /// Slots that traversed without touching the cache.
    pub off: usize,
    /// Slots whose header region was (re)allocated this frame.
    pub reallocated: usize,
}

/// First-fit allocator over one flat pool. Slots are few (≤ 512) so a sorted
/// free list is plenty.
#[derive(Clone, Debug)]
struct RegionAllocator {
    capacity: u32,
    /// Sorted, coalesced `(start, length)` free spans.
    free: Vec<(u32, u32)>,
}

impl RegionAllocator {
    fn new(capacity: u32) -> Self {
        Self {
            capacity,
            free: if capacity > 0 {
                vec![(0, capacity)]
            } else {
                Vec::new()
            },
        }
    }

    fn allocate(&mut self, length: u32) -> Option<u32> {
        if length == 0 {
            return None;
        }
        let index = self.free.iter().position(|(_, len)| *len >= length)?;
        let (start, len) = self.free[index];
        if len == length {
            self.free.remove(index);
        } else {
            self.free[index] = (start + length, len - length);
        }
        Some(start)
    }

    fn release(&mut self, start: u32, length: u32) {
        if length == 0 {
            return;
        }
        let index = self
            .free
            .iter()
            .position(|(s, _)| *s > start)
            .unwrap_or(self.free.len());
        self.free.insert(index, (start, length));
        // Coalesce with the neighbour after, then before.
        if index + 1 < self.free.len() && self.free[index].0 + self.free[index].1 == self.free[index + 1].0 {
            let next = self.free.remove(index + 1);
            self.free[index].1 += next.1;
        }
        if index > 0 && self.free[index - 1].0 + self.free[index - 1].1 == self.free[index].0 {
            let this = self.free.remove(index);
            self.free[index - 1].1 += this.1;
        }
    }

    fn reset(&mut self) {
        *self = Self::new(self.capacity);
    }
}

/// Per-renderer cache bookkeeping.
#[derive(Clone, Debug)]
pub(crate) struct IntervalCacheState {
    allocator: RegionAllocator,
    slots: Vec<SlotState>,
    frame: Option<FrameKey>,
    /// Slots whose region was compacted or freshly allocated this frame.
    pub(crate) reallocated: usize,
    stats: IntervalCacheStats,
    /// Planned-frame counter, 12 bits of which stamp payload claims.
    epoch: u32,
}

impl IntervalCacheState {
    pub(crate) fn new(header_words: u32) -> Self {
        Self {
            allocator: RegionAllocator::new(header_words),
            slots: Vec::new(),
            frame: None,
            reallocated: 0,
            stats: IntervalCacheStats::default(),
            epoch: 0,
        }
    }

    pub(crate) fn stats(&self) -> IntervalCacheStats {
        self.stats
    }

    /// The epoch of the last plan; never zero, so a zeroed claim cannot
    /// match a live frame.
    pub(crate) fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The pool was resized: nothing in it is trusted any more.
    pub(crate) fn resize(&mut self, header_words: u32) {
        self.allocator = RegionAllocator::new(header_words);
        self.slots.clear();
        self.frame = None;
    }

    /// A frame ran without the cached compute path (fragment haze, haze off,
    /// gobos): whatever the pool holds is no longer known to match anything.
    pub(crate) fn forget(&mut self) {
        self.frame = None;
        for slot in &mut self.slots {
            slot.key = None;
            slot.written = false;
        }
        self.stats = IntervalCacheStats::default();
    }

    /// Decide this frame's per-slot mode and region. `keys[s]` is `None` for
    /// a slot with no resident cone.
    ///
    /// Writes are gated on the frame key having settled: a frame whose camera
    /// or depth differs from the previous frame's traverses without writing,
    /// so a continuous orbit never pays for stores it could not reuse. The
    /// first still frame writes, the second reads. A per-slot change on a
    /// still camera (a moving head) writes immediately.
    pub(crate) fn plan(&mut self, frame: FrameKey, keys: &[Option<SlotKey>]) -> Vec<SlotEntry> {
        if self.slots.len() != keys.len() {
            self.allocator.reset();
            self.slots = vec![SlotState::default(); keys.len()];
            self.frame = None;
        }
        let same_frame = self.frame == Some(frame);
        self.frame = Some(frame);
        self.reallocated = 0;
        self.epoch = (self.epoch % 4095) + 1;
        if !same_frame {
            // Camera or depth changed: every region's contents are stale.
            for slot in &mut self.slots {
                slot.written = false;
            }
        }

        // Pass 1: keep what is still addressable, release what is not.
        let mut needs_region = Vec::new();
        let mut hits = vec![false; keys.len()];
        for (slot, key) in keys.iter().enumerate() {
            let state = &mut self.slots[slot];
            match key {
                None => {
                    if let Some((start, len)) = state.region.take() {
                        self.allocator.release(start, len);
                    }
                    state.key = None;
                }
                Some(key) => {
                    let same_key = state.key == Some(*key);
                    let sized = state.region.is_some_and(|(_, len)| len == key.blocks());
                    if same_key && sized {
                        hits[slot] = same_frame && state.written;
                    } else {
                        if let Some((start, len)) = state.region.take() {
                            self.allocator.release(start, len);
                        }
                        needs_region.push(slot);
                        state.written = false;
                    }
                    state.key = Some(*key);
                }
            }
        }

        // Pass 2: allocate for the changed slots; compact if the pool is
        // fragmented, which costs every slot one miss frame.
        let mut failed = false;
        for &slot in &needs_region {
            let blocks = keys[slot].expect("only keyed slots need regions").blocks();
            match self.allocator.allocate(blocks) {
                Some(start) => {
                    self.slots[slot].region = Some((start, blocks));
                    self.reallocated += 1;
                }
                None => failed = true,
            }
        }
        if failed {
            self.allocator.reset();
            hits.iter_mut().for_each(|hit| *hit = false);
            self.reallocated = 0;
            for (slot, key) in keys.iter().enumerate() {
                let state = &mut self.slots[slot];
                state.region = None;
                state.written = false;
                if let Some(key) = key {
                    state.region = self.allocator.allocate(key.blocks()).map(|start| (start, key.blocks()));
                    self.reallocated += 1;
                }
            }
        }

        self.stats = IntervalCacheStats {
            reallocated: self.reallocated,
            ..IntervalCacheStats::default()
        };
        let entries: Vec<SlotEntry> = keys
            .iter()
            .enumerate()
            .map(|(slot, key)| match (key, self.slots[slot].region) {
                (Some(key), Some((start, _))) if key.scatters && same_frame => {
                    let mode = if hits[slot] { MODE_READ } else { MODE_WRITE };
                    let state = &mut self.slots[slot];
                    if mode == MODE_WRITE {
                        state.gen = state.gen.wrapping_add(1) & 0x00FF_FFFF;
                        state.written = true;
                    }
                    [
                        start,
                        key.rect[0] | key.rect[1] << 16,
                        key.rect[2] | key.rect[3] << 16,
                        mode | state.gen << 8,
                    ]
                }
                _ => [0, 0, 0, MODE_OFF],
            })
            .collect();
        for entry in &entries {
            match entry[3] & 0xFF {
                MODE_READ => self.stats.read += 1,
                MODE_WRITE => self.stats.write += 1,
                _ => self.stats.off += 1,
            }
        }
        entries
    }
}

/// A light's tile rect, in full-resolution 8 px light-index tiles, mapped to
/// the haze target's 8×4 cache blocks. Conservative: every haze pixel whose
/// full-resolution footprint touches the rect lands in a returned block.
pub(crate) fn block_rect(
    tiles: [u32; 4],
    scale: [f32; 2],
    blocks: [u32; 2],
) -> [u32; 4] {
    let [x0, y0, x1, y1] = tiles;
    let lo = |tile: u32, scale: f32, size: u32| -> u32 {
        let pixel = f64::from(tile) * 8.0 / f64::from(scale.max(1e-6));
        ((pixel.floor().max(0.0) as u32) / size).min(u32::MAX)
    };
    let hi = |tile: u32, scale: f32, size: u32, count: u32| -> u32 {
        let pixel = f64::from(tile + 1) * 8.0 / f64::from(scale.max(1e-6));
        let last = (pixel.ceil().max(1.0) as u32).saturating_sub(1);
        (last / size).min(count.saturating_sub(1))
    };
    let bx0 = lo(x0, scale[0], 8).min(blocks[0].saturating_sub(1));
    let by0 = lo(y0, scale[1], 4).min(blocks[1].saturating_sub(1));
    let bx1 = hi(x1, scale[0], 8, blocks[0]).max(bx0);
    let by1 = hi(y1, scale[1], 4, blocks[1]).max(by0);
    [bx0, by0, bx1 - bx0 + 1, by1 - by0 + 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seed: u32) -> FrameKey {
        FrameKey {
            camera: [seed; 25],
            depth: 7,
        }
    }

    fn key(id: u32, rect: [u32; 4]) -> SlotKey {
        SlotKey {
            shadow: ShadowCacheKey {
                matrix_bits: [id; 16],
                caster_hash: 1,
            },
            range: 1,
            cos_field: 2,
            wash: 3,
            scatters: true,
            rect,
        }
    }

    fn modes(entries: &[SlotEntry]) -> Vec<u32> {
        entries.iter().map(|e| e[3] & 0xFF).collect()
    }

    #[test]
    fn every_write_frame_has_a_fresh_generation() {
        let mut state = IntervalCacheState::new(1000);
        let keys = vec![Some(key(1, [0, 0, 10, 10]))];
        state.plan(frame(1), &keys);
        let a = state.plan(frame(1), &keys)[0][3] >> 8;
        let b = state.plan(frame(1), &keys)[0][3] >> 8;
        state.plan(frame(2), &keys);
        let c = state.plan(frame(2), &keys)[0][3] >> 8;
        assert_eq!(a, b, "a read frame keeps the generation");
        assert_ne!(b, c, "a write frame bumps it");
    }

    #[test]
    fn cold_start_settles_then_writes_then_reads() {
        let mut state = IntervalCacheState::new(1000);
        let keys = vec![Some(key(1, [0, 0, 10, 10])), Some(key(2, [2, 3, 4, 5])), None];
        let cold = state.plan(frame(1), &keys);
        assert_eq!(modes(&cold), [MODE_OFF, MODE_OFF, MODE_OFF], "an unsettled frame never writes");
        let first = state.plan(frame(1), &keys);
        assert_eq!(modes(&first), [MODE_WRITE, MODE_WRITE, MODE_OFF]);
        assert_eq!(first[0][0], 0, "first region starts at the pool origin");
        assert_eq!(first[1][0], 100, "second region follows the first");
        assert_eq!(first[1][1], 2 | 3 << 16);
        assert_eq!(first[1][2], 4 | 5 << 16);
        assert_eq!(state.stats(), IntervalCacheStats { read: 0, write: 2, off: 1, reallocated: 0 });
        let second = state.plan(frame(1), &keys);
        assert_eq!(modes(&second), [MODE_READ, MODE_READ, MODE_OFF]);
        assert_eq!(second[0][0], 0);
        assert_eq!(second[1][0], 100, "a hit keeps its region");
    }

    #[test]
    fn camera_move_clears_every_slot_and_keeps_regions() {
        let mut state = IntervalCacheState::new(1000);
        let keys = vec![Some(key(1, [0, 0, 10, 10])), Some(key(2, [0, 0, 5, 5]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        let moved = state.plan(frame(2), &keys);
        assert_eq!(modes(&moved), [MODE_OFF, MODE_OFF], "the moving frame traverses without writing");
        assert_eq!(state.reallocated, 0);
        let settled = state.plan(frame(2), &keys);
        assert_eq!(modes(&settled), [MODE_WRITE, MODE_WRITE]);
        assert_eq!((settled[0][0], settled[1][0]), (0, 100));
        assert_eq!(modes(&state.plan(frame(2), &keys)), [MODE_READ, MODE_READ]);
        // A camera that never rests never writes.
        for i in 3..10 {
            assert_eq!(modes(&state.plan(frame(i), &keys)), [MODE_OFF, MODE_OFF]);
        }
    }

    #[test]
    fn one_light_move_clears_one_slot() {
        let mut state = IntervalCacheState::new(1000);
        let mut keys = vec![Some(key(1, [0, 0, 10, 10])), Some(key(2, [0, 0, 5, 5]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        keys[1] = Some(key(3, [0, 0, 5, 5]));
        let moved = state.plan(frame(1), &keys);
        assert_eq!(modes(&moved), [MODE_READ, MODE_WRITE]);
        assert_eq!(moved[0][0], 0, "the untouched slot keeps its region");
        // A same-size rect reuses the freed span; the first slot is intact.
        assert_eq!(moved[1][0], 100);
    }

    #[test]
    fn dimmer_change_clears_nothing() {
        // Intensity and colour are not inputs to the traversal, so they are
        // not in the key: two frames with identical keys are both hits even
        // though the caller's radiance changed.
        let mut state = IntervalCacheState::new(1000);
        let keys = vec![Some(key(1, [0, 0, 10, 10]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_READ]);
    }

    #[test]
    fn slot_eviction_and_reoccupation_write_again() {
        let mut state = IntervalCacheState::new(1000);
        let mut keys = vec![Some(key(1, [0, 0, 10, 10])), Some(key(2, [0, 0, 5, 5]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        keys[0] = None;
        let evicted = state.plan(frame(1), &keys);
        assert_eq!(modes(&evicted), [MODE_OFF, MODE_READ]);
        keys[0] = Some(key(9, [1, 1, 3, 3]));
        let back = state.plan(frame(1), &keys);
        assert_eq!(modes(&back), [MODE_WRITE, MODE_READ]);
        assert_eq!(back[0][0], 0, "the freed span at the origin is reused");
        // Same slot, same key as its previous tenant: still a miss, because
        // the slot was not resident with that key last frame.
        keys[0] = None;
        state.plan(frame(1), &keys);
        keys[0] = Some(key(9, [1, 1, 3, 3]));
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_WRITE, MODE_READ]);
    }

    #[test]
    fn a_rect_change_is_a_key_change() {
        let mut state = IntervalCacheState::new(1000);
        let mut keys = vec![Some(key(1, [0, 0, 10, 10]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        keys[0] = Some(key(1, [0, 0, 10, 11]));
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_WRITE]);
    }

    #[test]
    fn non_scattering_slots_are_off() {
        let mut state = IntervalCacheState::new(1000);
        let mut k = key(1, [0, 0, 10, 10]);
        k.scatters = false;
        let keys = vec![Some(k)];
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_OFF]);
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_OFF]);
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_OFF]);
    }

    #[test]
    fn pool_exhaustion_compacts_and_misses_everything_once() {
        let mut state = IntervalCacheState::new(250);
        let mut keys = vec![Some(key(1, [0, 0, 10, 10])), Some(key(2, [0, 0, 10, 10]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        // Free the first region, then ask for one that fits only at the origin
        // plus the tail: fragmentation forces a compaction.
        keys[0] = None;
        state.plan(frame(1), &keys);
        keys[0] = Some(key(3, [0, 0, 10, 15]));
        let compacted = state.plan(frame(1), &keys);
        assert_eq!(modes(&compacted), [MODE_WRITE, MODE_WRITE]);
        assert_eq!(state.reallocated, 2);
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_READ, MODE_READ]);
        // A region that cannot fit even after compaction is simply off.
        keys[1] = Some(key(4, [0, 0, 20, 20]));
        let too_big = state.plan(frame(1), &keys);
        assert_eq!(modes(&too_big), [MODE_WRITE, MODE_OFF]);
    }

    #[test]
    fn forgetting_forces_a_full_miss() {
        let mut state = IntervalCacheState::new(1000);
        let keys = vec![Some(key(1, [0, 0, 10, 10]))];
        state.plan(frame(1), &keys);
        state.plan(frame(1), &keys);
        state.forget();
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_OFF]);
        assert_eq!(modes(&state.plan(frame(1), &keys)), [MODE_WRITE]);
    }

    #[test]
    fn allocator_coalesces_freed_neighbours() {
        let mut allocator = RegionAllocator::new(100);
        let a = allocator.allocate(30).unwrap();
        let b = allocator.allocate(30).unwrap();
        let c = allocator.allocate(30).unwrap();
        allocator.release(a, 30);
        allocator.release(c, 30);
        assert_eq!(allocator.free, vec![(0, 30), (60, 40)]);
        allocator.release(b, 30);
        assert_eq!(allocator.free, vec![(0, 100)]);
    }

    #[test]
    fn block_rects_cover_the_tile_footprint() {
        // Full resolution haze: 8 px tiles are 8 px blocks, two block rows
        // per tile row.
        assert_eq!(block_rect([3, 2, 5, 4], [1.0, 1.0], [279, 348]), [3, 4, 3, 6]);
        assert_eq!(block_rect([0, 0, 278, 173], [1.0, 1.0], [279, 348]), [0, 0, 279, 348]);
        // Half-resolution haze: a 16 px full-res span is 8 haze px.
        assert_eq!(block_rect([2, 2, 3, 3], [2.0, 2.0], [100, 100]), [1, 2, 1, 2]);
        // Clamped to the block grid.
        assert_eq!(block_rect([0, 0, 500, 500], [1.0, 1.0], [10, 10]), [0, 0, 10, 10]);
    }
}
