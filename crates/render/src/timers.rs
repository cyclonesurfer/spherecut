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
/// been polled. A span whose ring entry is still busy isn't timed.
#[derive(Debug)]
pub struct GpuTimers {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    entries: Vec<(wgpu::Buffer, Arc<AtomicU8>)>,
    period_ns: f32,
    latest: Option<f32>,
}

/// A span begun with [`GpuTimers::begin`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerSpan(usize);

impl GpuTimers {
    pub const RING: usize = 4;
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
            .map(|_| {
                let b = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: 16,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                (b, Arc::new(AtomicU8::new(FREE)))
            })
            .collect();
        Some(GpuTimers {
            set,
            resolve,
            entries,
            period_ns: gpu.caps.timestamp_period_ns,
            latest: None,
        })
    }

    /// Starts a span: a timestamp now. `None` when every entry of the ring
    /// is still waiting for the GPU.
    pub fn begin(&mut self, enc: &mut wgpu::CommandEncoder) -> Option<TimerSpan> {
        self.poll();
        let i = self
            .entries
            .iter()
            .position(|(_, s)| s.load(Ordering::Acquire) == FREE)?;
        self.entries[i].1.store(IN_FLIGHT, Ordering::Release);
        enc.write_timestamp(&self.set, 2 * i as u32);
        Some(TimerSpan(i))
    }

    /// Ends a span, and has its timestamps read back once `enc` is
    /// submitted.
    pub fn end(&mut self, enc: &mut wgpu::CommandEncoder, span: TimerSpan) {
        let i = span.0;
        let Some((buffer, state)) = self.entries.get(i) else {
            return;
        };
        enc.write_timestamp(&self.set, 2 * i as u32 + 1);
        let at = Self::STRIDE * i as u64;
        enc.resolve_query_set(&self.set, 2 * i as u32..2 * i as u32 + 2, &self.resolve, at);
        enc.copy_buffer_to_buffer(&self.resolve, at, buffer, 0, 16);
        let state = state.clone();
        enc.map_buffer_on_submit(buffer, wgpu::MapMode::Read, .., move |r| {
            state.store(if r.is_ok() { READY } else { FREE }, Ordering::Release);
        });
    }

    /// Reads every span the GPU has finished, and returns the latest, in
    /// milliseconds. The device must have been polled for the read-backs
    /// to arrive; this doesn't wait.
    pub fn poll(&mut self) -> Option<f32> {
        for (buffer, state) in &self.entries {
            if state.load(Ordering::Acquire) != READY {
                continue;
            }
            if let Ok(view) = buffer.get_mapped_range(..) {
                let t: [u64; 2] = bytemuck::pod_read_unaligned(&view[..16]);
                drop(view);
                let ticks = t[1].saturating_sub(t[0]);
                self.latest = Some(ticks as f32 * self.period_ns / 1e6);
            }
            buffer.unmap();
            state.store(FREE, Ordering::Release);
        }
        self.latest
    }

    /// The latest span read, in milliseconds, without collecting.
    pub fn latest_ms(&self) -> Option<f32> {
        self.latest
    }
}
