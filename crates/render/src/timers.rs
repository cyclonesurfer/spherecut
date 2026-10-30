//! GPU time between two points of a command encoder, read back without
//! waiting (M1 plan section 4.4): the upload's ingest and mips, and the
//! projection, for the overlay and `bench_upload_5k7`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::gpu::Gpu;

const FREE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const READY: u8 = 2;

/// Timestamp pairs in a ring of [`GpuTimers::RING`] read-back buffers.
/// [`GpuTimers::begin`] and [`GpuTimers::end`] bracket work in an
/// encoder; the read-back maps when the encoder is submitted, and
/// [`GpuTimers::poll`] collects what has finished, after the device has
/// been polled. A span whose ring entry is still busy isn't timed. An
/// encoder dropped unsubmitted (egui-wgpu drops its frame's when the
/// surface can't be had) never maps its entry's buffer, so an entry
/// still busy [`GpuTimers::LOST_AFTER`] begins later is taken for lost
/// and gets a new buffer.
#[derive(Debug)]
pub struct GpuTimers {
    device: wgpu::Device,
    label: String,
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    entries: Vec<Entry>,
    period_ns: f32,
    latest: Option<f32>,
    /// The newest reading not yet returned by [`GpuTimers::poll`].
    fresh: Option<f32>,
    /// Calls to [`GpuTimers::begin`] so far.
    begins: u64,
}

#[derive(Debug)]
struct Entry {
    buffer: wgpu::Buffer,
    state: Arc<AtomicU8>,
    /// The begin it was taken at.
    begun: u64,
}

impl Entry {
    fn new(device: &wgpu::Device, label: &str) -> Entry {
        Entry {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: 16,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            state: Arc::new(AtomicU8::new(FREE)),
            begun: 0,
        }
    }
}

/// A span begun with [`GpuTimers::begin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerSpan(usize);

impl GpuTimers {
    pub const RING: usize = 4;
    /// Begins after which an entry still in flight is taken for lost: a
    /// second of frames at 60 Hz, where a submitted span reads back in
    /// one or two.
    pub const LOST_AFTER: u64 = 64;
    /// `resolve_query_set` writes at multiples of this.
    const STRIDE: u64 = wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;

    /// `None` when the device can't write timestamps between passes
    /// (`TIMESTAMP_QUERY_INSIDE_ENCODERS`).
    pub fn new(gpu: &Gpu, label: &str) -> Option<GpuTimers> {
        if !gpu.caps.timestamps_in_encoders {
            return None;
        }
        let ring = Self::RING as u64;
        let set = gpu.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some(label),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * Self::RING as u32,
        });
        let resolve = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: Self::STRIDE * ring,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let entries = (0..Self::RING)
            .map(|_| Entry::new(&gpu.device, label))
            .collect();
        Some(GpuTimers {
            device: gpu.device.clone(),
            label: label.to_owned(),
            set,
            resolve,
            entries,
            period_ns: gpu.caps.timestamp_period_ns,
            latest: None,
            fresh: None,
            begins: 0,
        })
    }

    /// Starts a span: a timestamp now. `None` when every entry of the ring
    /// is still waiting for the GPU.
    pub fn begin(&mut self, enc: &mut wgpu::CommandEncoder) -> Option<TimerSpan> {
        self.collect();
        self.begins += 1;
        let free = self
            .entries
            .iter()
            .position(|e| e.state.load(Ordering::Acquire) == FREE);
        let i = match free {
            Some(i) => i,
            None => {
                let lost = self.entries.iter().position(|e| {
                    e.state.load(Ordering::Acquire) == IN_FLIGHT
                        && e.begun + Self::LOST_AFTER <= self.begins
                })?;
                // Its read-back may still come, to the old buffer and
                // state, which nothing reads now.
                self.entries[lost] = Entry::new(&self.device, &self.label);
                lost
            }
        };
        let e = &mut self.entries[i];
        e.state.store(IN_FLIGHT, Ordering::Release);
        e.begun = self.begins;
        enc.write_timestamp(&self.set, 2 * i as u32);
        Some(TimerSpan(i))
    }

    /// Ends a span, and has its timestamps read back once `enc` is
    /// submitted.
    pub fn end(&mut self, enc: &mut wgpu::CommandEncoder, span: TimerSpan) {
        let i = span.0;
        let Some(e) = self.entries.get(i) else {
            return;
        };
        enc.write_timestamp(&self.set, 2 * i as u32 + 1);
        let at = Self::STRIDE * i as u64;
        enc.resolve_query_set(&self.set, 2 * i as u32..2 * i as u32 + 2, &self.resolve, at);
        enc.copy_buffer_to_buffer(&self.resolve, at, &e.buffer, 0, 16);
        let state = e.state.clone();
        enc.map_buffer_on_submit(&e.buffer, wgpu::MapMode::Read, .., move |r| {
            state.store(if r.is_ok() { READY } else { FREE }, Ordering::Release);
        });
    }

    /// Reads every span the GPU has finished, and returns the newest of
    /// them, in milliseconds: `None` when none has finished since the
    /// last call, so that each reading is returned once. The device must
    /// have been polled for the read-backs to arrive; this doesn't wait.
    pub fn poll(&mut self) -> Option<f32> {
        self.collect();
        self.fresh.take()
    }

    /// Reads what has finished into `fresh` and `latest`.
    fn collect(&mut self) {
        let mut newest: Option<(u64, f32)> = None;
        for e in &self.entries {
            if e.state.load(Ordering::Acquire) != READY {
                continue;
            }
            if let Ok(view) = e.buffer.get_mapped_range(..) {
                let t: [u64; 2] = bytemuck::pod_read_unaligned(&view[..16]);
                drop(view);
                let ms = t[1].saturating_sub(t[0]) as f32 * self.period_ns / 1e6;
                if newest.is_none_or(|(b, _)| e.begun > b) {
                    newest = Some((e.begun, ms));
                }
            }
            e.buffer.unmap();
            e.state.store(FREE, Ordering::Release);
        }
        if let Some((_, ms)) = newest {
            self.latest = Some(ms);
            self.fresh = Some(ms);
        }
    }

    /// The latest span read, in milliseconds, without collecting.
    pub fn latest_ms(&self) -> Option<f32> {
        self.latest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submit_and_wait(gpu: &Gpu, enc: wgpu::CommandEncoder) {
        gpu.queue.submit([enc.finish()]);
        let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    }

    /// Spans whose encoders were dropped unsubmitted come back after
    /// [`GpuTimers::LOST_AFTER`] begins, and a reading is returned once.
    #[test]
    fn a_lost_span_frees_its_entry_and_readings_come_once() {
        let gpu = Gpu::headless(&crate::AdapterChoice::Cpu).unwrap();
        let mut timers = GpuTimers::new(&gpu, "test").expect("lavapipe has timestamps");
        let encoder = || {
            gpu.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default())
        };
        // One read, once.
        let mut enc = encoder();
        let span = timers.begin(&mut enc).unwrap();
        timers.end(&mut enc, span);
        submit_and_wait(&gpu, enc);
        assert!(timers.poll().is_some());
        assert_eq!(timers.poll(), None);
        assert!(timers.latest_ms().is_some());
        // The whole ring lost to dropped encoders.
        for _ in 0..GpuTimers::RING {
            let mut enc = encoder();
            let span = timers.begin(&mut enc).unwrap();
            timers.end(&mut enc, span);
            drop(enc);
        }
        let mut tries = 0;
        let span = loop {
            tries += 1;
            assert!(tries <= GpuTimers::LOST_AFTER, "no entry came back");
            let mut enc = encoder();
            if let Some(span) = timers.begin(&mut enc) {
                break (enc, span);
            }
        };
        assert!(tries > 1, "a lost entry was taken at once");
        let (mut enc, span) = span;
        timers.end(&mut enc, span);
        submit_and_wait(&gpu, enc);
        assert!(timers.poll().is_some(), "the new entry reads back");
    }
}
