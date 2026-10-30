//! The chunk ring between the audio decoder and the output's callback (M1
//! plan section 9.2): single producer, single consumer, in safe Rust.
//!
//! The ring holds about half a second in chunks of [`CHUNK_FRAMES`]
//! frames. A sample is an `AtomicU32` holding an f32's bits, and each
//! chunk has an atomic header: the epoch it was decoded for, its first
//! sample's index on the clip's axis at the output rate, and its frame
//! count. The producer writes a chunk and then publishes it by moving
//! `head` on (Release); the consumer reads `head` (Acquire) and then the
//! chunk. Neither waits: a full ring is the producer's to retry (the
//! decode thread sleeps 5 ms), an empty one the consumer's to fill with
//! silence.
//!
//! **Placement.** The consumer asks for samples by their index, not for
//! the next ones in the ring: it drops chunks of another epoch and chunks
//! that end before the index (an underrun's debt, plan section 9.2),
//! gives silence before a chunk that starts after it, and copies the
//! rest. So it keeps no state of its own, a seek flushes by bumping the
//! epoch alone, and after an underrun the next sample heard is the one
//! due at that time.
//!
//! **Flushing while paused.** A paused output's callback may not run, so
//! stale chunks could fill the ring for good. When the ring is full the
//! producer may reclaim the oldest chunk if it is of another epoch: both
//! sides move `tail` by compare-and-swap. A consumer copies only from a
//! chunk of the epoch it plays, and the producer reclaims only chunks of
//! an older one, so a reclaimed chunk is never one being played (except
//! across a seek's first callback, whose samples are the old epoch's
//! anyway). The producer writes a reclaimed slot after a release fence,
//! and the consumer reads `tail` again after an acquire fence once it
//! has read a chunk's header, and again once it has copied its samples:
//! if it saw anything written after the reclaim, it finds `tail` moved,
//! and acts on nothing it read.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering, fence};

/// Frames in a chunk.
pub const CHUNK_FRAMES: usize = 512;

/// About how much the ring holds, in milliseconds.
pub const RING_MS: usize = 500;

struct Chunk {
    epoch: AtomicU64,
    start: AtomicI64,
    frames: AtomicU32,
    samples: Box<[AtomicU32]>,
}

/// The ring (see the module docs).
pub struct ChunkRing {
    channels: usize,
    chunks: Box<[Chunk]>,
    /// Chunks written, ever.
    head: AtomicU64,
    /// Chunks consumed or reclaimed, ever.
    tail: AtomicU64,
}

impl std::fmt::Debug for ChunkRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkRing")
            .field("channels", &self.channels)
            .field("chunks", &self.chunks.len())
            .field("queued", &self.queued())
            .finish()
    }
}

/// What [`ChunkRing::fill`] gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Fill {
    /// Frames copied from chunks of the epoch.
    pub copied: usize,
    /// Frames of silence before a chunk that starts later (a gap).
    pub gap: usize,
    /// Frames of silence at the end, for want of a chunk: the ring ran
    /// dry there.
    pub dry: usize,
}

impl ChunkRing {
    /// A ring of about half a second at `rate` of `channels` channels.
    pub fn new(rate: u32, channels: u16) -> Self {
        let frames = rate as usize * RING_MS / 1000;
        Self::with_chunks(frames.div_ceil(CHUNK_FRAMES).max(2), channels)
    }

    /// A ring of `chunks` chunks.
    pub fn with_chunks(chunks: usize, channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        let chunks = (0..chunks.max(1))
            .map(|_| Chunk {
                epoch: AtomicU64::new(0),
                start: AtomicI64::new(0),
                frames: AtomicU32::new(0),
                samples: (0..CHUNK_FRAMES * channels)
                    .map(|_| AtomicU32::new(0))
                    .collect(),
            })
            .collect();
        ChunkRing {
            channels,
            chunks,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// How many chunks it holds.
    pub fn capacity(&self) -> usize {
        self.chunks.len()
    }

    /// Chunks waiting, of any epoch.
    pub fn queued(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        usize::try_from(head.saturating_sub(tail)).unwrap_or(usize::MAX)
    }

    fn slot(&self, n: u64) -> &Chunk {
        &self.chunks[(n % self.chunks.len() as u64) as usize]
    }

    /// Producer: write up to [`CHUNK_FRAMES`] frames of `data`
    /// (interleaved) as one chunk of `epoch` starting at sample `start`.
    /// Returns the frames written: 0 when the ring is full of chunks of
    /// `epoch` (or newer). A full ring whose oldest chunk is of an older
    /// epoch gives that chunk up first.
    pub fn push(&self, epoch: u64, start: i64, data: &[f32]) -> usize {
        let frames = (data.len() / self.channels).min(CHUNK_FRAMES);
        if frames == 0 {
            return 0;
        }
        loop {
            let head = self.head.load(Ordering::Relaxed);
            let tail = self.tail.load(Ordering::Acquire);
            if head.saturating_sub(tail) < self.chunks.len() as u64 {
                // The slot may be one just reclaimed, which the consumer
                // may still be reading: it reads `tail` again after an
                // acquire fence, and finds it moved if it saw any of
                // this.
                fence(Ordering::Release);
                let c = self.slot(head);
                for (cell, &v) in c.samples.iter().zip(&data[..frames * self.channels]) {
                    cell.store(v.to_bits(), Ordering::Relaxed);
                }
                c.epoch.store(epoch, Ordering::Relaxed);
                c.start.store(start, Ordering::Relaxed);
                c.frames.store(frames as u32, Ordering::Relaxed);
                self.head.store(head + 1, Ordering::Release);
                return frames;
            }
            // Full: reclaim the oldest chunk if it is stale.
            if self.slot(tail).epoch.load(Ordering::Acquire) >= epoch {
                return 0;
            }
            let _ = self
                .tail
                .compare_exchange(tail, tail + 1, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// Consumer: fill `out` (interleaved, `frames` frames of the ring's
    /// channels) with the samples of `epoch` from index `from` on, by
    /// placement (see the module docs). Chunks before `from`, and of
    /// another epoch, are dropped on the way.
    pub fn fill(&self, out: &mut [f32], from: i64, epoch: u64) -> Fill {
        let ch = self.channels;
        let frames = out.len() / ch;
        let mut fill = Fill::default();
        let mut i = 0usize;
        while i < frames {
            let pos = from + i as i64;
            let tail = self.tail.load(Ordering::Acquire);
            let head = self.head.load(Ordering::Acquire);
            if tail >= head {
                out[i * ch..frames * ch].fill(0.0);
                fill.dry = frames - i;
                break;
            }
            let c = self.slot(tail);
            let (e, start, n) = (
                c.epoch.load(Ordering::Relaxed),
                c.start.load(Ordering::Relaxed),
                i64::from(c.frames.load(Ordering::Relaxed)),
            );
            // The producer may have reclaimed the chunk and be writing
            // another's header over it: with `tail` where it was, the
            // header is the chunk's (see `push`'s fence).
            fence(Ordering::Acquire);
            if self.tail.load(Ordering::Relaxed) != tail {
                continue;
            }
            if e != epoch || start + n <= pos {
                let _ =
                    self.tail
                        .compare_exchange(tail, tail + 1, Ordering::AcqRel, Ordering::Acquire);
                continue;
            }
            if start > pos {
                let gap = usize::try_from(start - pos)
                    .unwrap_or(usize::MAX)
                    .min(frames - i);
                out[i * ch..(i + gap) * ch].fill(0.0);
                fill.gap += gap;
                i += gap;
                continue;
            }
            let offset = usize::try_from(pos - start).unwrap_or(0);
            let take = (usize::try_from(n).unwrap_or(0) - offset).min(frames - i);
            for (o, cell) in out[i * ch..(i + take) * ch]
                .iter_mut()
                .zip(&c.samples[offset * ch..(offset + take) * ch])
            {
                *o = f32::from_bits(cell.load(Ordering::Relaxed));
            }
            // A chunk reclaimed while being read was of an older epoch
            // than the producer's, so of no use: say silence.
            fence(Ordering::Acquire);
            if self.tail.load(Ordering::Relaxed) != tail {
                out[i * ch..(i + take) * ch].fill(0.0);
                continue;
            }
            fill.copied += take;
            i += take;
            if offset + take == usize::try_from(n).unwrap_or(0) {
                let _ =
                    self.tail
                        .compare_exchange(tail, tail + 1, Ordering::AcqRel, Ordering::Acquire);
            }
        }
        fill
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn frames(start: i64, n: usize) -> Vec<f32> {
        (0..n as i64)
            .flat_map(|i| [(start + i) as f32, -((start + i) as f32)])
            .collect()
    }

    #[test]
    fn samples_come_out_by_their_place() {
        let ring = ChunkRing::with_chunks(4, 2);
        assert_eq!(ring.push(1, 0, &frames(0, 512)), 512);
        assert_eq!(ring.push(1, 512, &frames(512, 512)), 512);
        let mut out = vec![9.0; 2 * 100];
        // From 300: the first chunk's tail, then the second's head.
        assert_eq!(
            ring.fill(&mut out[..2 * 50], 300, 1),
            Fill {
                copied: 50,
                ..Fill::default()
            }
        );
        assert_eq!(out[..2 * 50], frames(300, 50)[..]);
        // Asking from 1000 drops the first chunk (debt) and reads to the
        // second's end, then runs dry.
        let f = ring.fill(&mut out, 1000, 1);
        assert_eq!((f.copied, f.dry), (24, 76));
        assert_eq!(out[..48], frames(1000, 24)[..]);
        assert!(out[48..].iter().all(|&x| x == 0.0));
        assert_eq!(ring.queued(), 0);
    }

    #[test]
    fn a_gap_before_a_chunk_is_silence_and_other_epochs_are_dropped() {
        let ring = ChunkRing::with_chunks(4, 2);
        ring.push(1, 0, &frames(0, 512));
        ring.push(2, 100, &frames(100, 512));
        let mut out = vec![9.0; 2 * 200];
        let f = ring.fill(&mut out, 0, 2);
        assert_eq!((f.gap, f.copied, f.dry), (100, 100, 0));
        assert!(out[..200].iter().all(|&x| x == 0.0));
        assert_eq!(out[200..], frames(100, 100)[..]);
    }

    #[test]
    fn a_full_ring_of_an_older_epoch_makes_room() {
        let ring = ChunkRing::with_chunks(2, 1);
        let one = vec![1.0; 512];
        assert_eq!(ring.push(1, 0, &one), 512);
        assert_eq!(ring.push(1, 512, &one), 512);
        assert_eq!(ring.push(1, 1024, &one), 0, "full of its own epoch");
        assert_eq!(
            ring.push(2, 0, &[2.0; 512]),
            512,
            "an older chunk gives way"
        );
        assert_eq!(ring.push(2, 512, &[2.0; 512]), 512);
        assert_eq!(ring.push(2, 1024, &[2.0; 512]), 0);
        let mut out = vec![0.0; 1024];
        assert_eq!(ring.fill(&mut out, 0, 2).copied, 1024);
        assert!(out.iter().all(|&x| x == 2.0));
    }

    /// 10⁷ samples through the ring between two threads, in chunks of
    /// odd sizes, with epochs flushed on the way: what comes out of an
    /// epoch is in order, nothing of it is lost while it plays, and
    /// nothing of another epoch comes out.
    #[test]
    fn two_threads_keep_order_and_lose_nothing_but_flushed_epochs() {
        const TOTAL: i64 = 10_000_000;
        let ring = Arc::new(ChunkRing::with_chunks(8, 1));
        let epoch = Arc::new(AtomicU64::new(1));
        // Where a new epoch starts: the consumer's place when it flushed.
        let restart = Arc::new(AtomicI64::new(0));
        let producer = {
            let (ring, epoch, restart) = (ring.clone(), epoch.clone(), restart.clone());
            std::thread::spawn(move || {
                let mut pos = 0i64;
                let mut e = 1;
                let mut size = 7usize;
                while pos < TOTAL {
                    let now = epoch.load(Ordering::Acquire);
                    if now != e {
                        // A seek: start again from where the consumer
                        // will ask, as the decoder seeks there.
                        e = now;
                        pos = restart.load(Ordering::Acquire);
                    }
                    size = size % 509 + 3;
                    let n = size.min((TOTAL - pos) as usize);
                    let data: Vec<f32> = (0..n as i64).map(|i| (pos + i) as f32).collect();
                    let w = ring.push(e, pos, &data);
                    if w == 0 {
                        std::thread::yield_now();
                    }
                    pos += w as i64;
                }
            })
        };
        let mut next = 0i64;
        let mut out = vec![0.0f32; 300];
        let mut flushes = 0;
        while next < TOTAL {
            let e = epoch.load(Ordering::Acquire);
            let want = out.len().min((TOTAL - next) as usize);
            let f = ring.fill(&mut out[..want], next, e);
            for (k, &v) in out[..f.copied].iter().enumerate() {
                assert_eq!(v, (next + k as i64) as f32, "at {}", next + k as i64);
            }
            assert_eq!(f.gap, 0);
            next += f.copied as i64;
            if f.copied == 0 {
                std::thread::yield_now();
            }
            if next / 1_000_000 > flushes {
                flushes += 1;
                restart.store(next, Ordering::Release);
                epoch.fetch_add(1, Ordering::AcqRel);
            }
        }
        producer.join().unwrap();
        assert!(flushes >= 9);
    }

    /// A seek's first callbacks race the producer reclaiming the old
    /// epoch's chunks: the producer may write the new epoch's header into
    /// a slot whose old header the consumer is reading. The new epoch's
    /// audio starts where the consumer asks, so it must never get a gap,
    /// nor anything but its samples. Before `fill` read `tail` again
    /// after a chunk's header, 1 000 000 rounds of this in a release
    /// build gave a gap 3 and 5 times; after, none in 3 000 000. Here it
    /// runs far fewer, as a guard.
    #[test]
    fn a_reclaim_racing_the_first_fill_of_an_epoch_leaves_no_gap() {
        const ROUNDS: u64 = 20_000;
        for round in 0..ROUNDS {
            let ring = Arc::new(ChunkRing::with_chunks(4, 1));
            let (old, new) = (2 * round + 1, 2 * round + 2);
            // Full of the old epoch, starting far after the new audio,
            // so that a header torn between the two reads as a gap.
            for k in 0..4 {
                assert_eq!(ring.push(old, 1_000_000 + 512 * k, &[7.0; 512]), 512);
            }
            let barrier = Arc::new(std::sync::Barrier::new(2));
            let producer = {
                let (ring, barrier) = (ring.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut pos = 0i64;
                    while pos < 512 * 4 {
                        let v: Vec<f32> = (0..512).map(|i| (pos + i) as f32).collect();
                        let n = ring.push(new, pos, &v);
                        if n == 0 {
                            std::hint::spin_loop();
                        }
                        pos += n as i64;
                    }
                })
            };
            barrier.wait();
            let mut out = [0.0f32; 64];
            let mut next = 0i64;
            while next < 512 * 4 {
                let f = ring.fill(&mut out, next, new);
                assert_eq!(f.gap, 0, "round {round}: a gap at {next}");
                for (k, &v) in out[..f.copied].iter().enumerate() {
                    assert_eq!(v, (next + k as i64) as f32, "round {round}");
                }
                next += f.copied as i64;
            }
            producer.join().unwrap();
        }
    }
}
