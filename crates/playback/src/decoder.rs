//! Which decoder a stream plays on (M1 plan section 8.2, amendment 9,
//! DECISIONS D34): software, or a hardware kind picked from what doctor's
//! decode checks found on this machine rather than from a fixed order.
//!
//! [`DecoderPreference::Auto`] takes the stream's format (its codec, depth
//! and chroma, which [`doctor::check_format`] maps to one of doctor's
//! checks) and the kinds whose check of it works:
//!
//! - none: software, saying why;
//! - one: that kind;
//! - both: Vulkan Video on a discrete GPU and VA-API otherwise. On odin's
//!   discrete RX 7600, VA-API with its copy out of video memory decoded
//!   the X5's modes at 0.81 to 1.29 times real time and Vulkan Video at
//!   1.61 to 2.23 (amendment 9); on huginn's unified memory VA-API keeps
//!   up, and it is the path the Tier 2 checks hold bit-exact to software
//!   (D30).
//!
//! Either way the hardware decode falls back to software as
//! [`Fallback::Software`] does, and says so. A format no check covers
//! decodes in software. [`DecoderPreference::Only`] decodes on one kind
//! and fails rather than fall back, as `spherecut frame --decoder vaapi`
//! does (D39).

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use spherecut_core::may_block;
use spherecut_media::backend::{DecodeMode, Fallback, HwKind};
use spherecut_media::doctor::{self, CheckResult, DoctorReport};
use spherecut_media::probe::{ProbeReport, StreamInfo, StreamKind};

/// How the player picks a stream's decoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DecoderPreference {
    /// From doctor's decode checks (module docs), falling back to
    /// software.
    #[default]
    Auto,
    /// Software only.
    Software,
    /// On this kind of device only: a device that can't decode the stream
    /// fails it.
    Only(HwKind),
}

/// One of doctor's decode checks on a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwDecodeCheck {
    /// As doctor names it: "HEVC Main 10".
    pub format: String,
    pub kind: HwKind,
    pub result: CheckResult,
    /// Why it fails or wasn't tested.
    pub reason: Option<String>,
}

/// What doctor's decode checks found on the devices: what
/// [`DecoderPreference::Auto`] decides by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HwSupport {
    pub checks: Vec<HwDecodeCheck>,
}

impl HwSupport {
    /// The hardware checks of a doctor report (the GUI's startup check).
    pub fn from_report(report: &DoctorReport) -> HwSupport {
        let checks = report
            .decode_checks
            .iter()
            .filter_map(|c| {
                let kind = match c.path.as_str() {
                    "vaapi" => HwKind::VaApi,
                    "vulkan" => HwKind::Vulkan,
                    _ => return None,
                };
                Some(HwDecodeCheck {
                    format: c.format.clone(),
                    kind,
                    result: c.result,
                    reason: c.reason.clone(),
                })
            })
            .collect();
        HwSupport { checks }
    }

    /// Run doctor's checks on `render_node`. Blocks (opening devices and
    /// decoding a frame on each): on a worker, never the UI thread.
    pub fn check(render_node: &Path) -> HwSupport {
        Self::from_report(&doctor::run(render_node))
    }

    /// The check of `format` on `kind`.
    pub fn get(&self, format: &str, kind: HwKind) -> Option<&HwDecodeCheck> {
        self.checks
            .iter()
            .find(|c| c.format == format && c.kind == kind)
    }

    pub fn works(&self, format: &str, kind: HwKind) -> bool {
        self.get(format, kind)
            .is_some_and(|c| c.result == CheckResult::Works)
    }
}

/// Doctor's checks that another thread is running (the GUI's startup
/// check), for the opens that need them: the first waits for them rather
/// than running them a second time on the same devices.
#[derive(Debug, Clone)]
pub struct HwPromise(Arc<Promised>);

/// The other end of an [`HwPromise`], for the thread running the checks.
/// Dropped without [`keep`](HwKeeper::keep) (the checks panicked), it
/// breaks the promise, and the open waiting runs them itself.
#[derive(Debug)]
pub struct HwKeeper(Option<Arc<Promised>>);

#[derive(Debug, Default)]
struct Promised {
    state: Mutex<Kept>,
    changed: Condvar,
}

#[derive(Debug, Default)]
enum Kept {
    #[default]
    Pending,
    Given(HwSupport),
    Broken,
}

impl HwPromise {
    pub fn new() -> (HwKeeper, HwPromise) {
        let p = Arc::new(Promised::default());
        (HwKeeper(Some(p.clone())), HwPromise(p))
    }

    /// The checks, once kept; `None` if the promise broke or `limit`
    /// passed first. Blocks: on a worker, never the UI thread.
    #[track_caller]
    pub fn wait(&self, limit: Duration) -> Option<HwSupport> {
        may_block("waiting for doctor's checks");
        let deadline = Instant::now() + limit;
        let mut g = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match &*g {
                Kept::Given(s) => return Some(s.clone()),
                Kept::Broken => return None,
                Kept::Pending => {}
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .0
                .changed
                .wait_timeout(g, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

impl HwKeeper {
    pub fn keep(mut self, support: HwSupport) {
        if let Some(p) = self.0.take() {
            p.settle(Kept::Given(support));
        }
    }
}

impl Drop for HwKeeper {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            p.settle(Kept::Broken);
        }
    }
}

impl Promised {
    fn settle(&self, to: Kept) {
        *self.state.lock().unwrap_or_else(PoisonError::into_inner) = to;
        self.changed.notify_all();
    }
}

/// What a stream is, as far as the choice cares: from probe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamFormat {
    pub codec: String,
    pub bit_depth: Option<u32>,
    pub chroma_log2: Option<(u32, u32)>,
}

impl StreamFormat {
    /// The format of video stream `index` in `report`, or with `None` the
    /// one FFmpeg would pick: the first video stream marked default, else
    /// the first video stream. (FFmpeg's best stream ranks by more than
    /// that, but a file whose video streams differ in format is rare: the
    /// X5's two lenses are alike.)
    pub fn of(report: &ProbeReport, index: Option<usize>) -> Option<StreamFormat> {
        let video = |s: &&StreamInfo| s.kind == StreamKind::Video;
        let s = match index {
            Some(i) => report.streams.iter().find(|s| s.index == i),
            None => report
                .streams
                .iter()
                .filter(video)
                .find(|s| s.disposition.iter().any(|d| d == "default"))
                .or_else(|| report.streams.iter().find(video)),
        }?;
        let v = s.video.as_ref()?;
        Some(StreamFormat {
            codec: s.codec.clone()?,
            bit_depth: v.bit_depth,
            chroma_log2: v.chroma_subsampling,
        })
    }

    /// The format of doctor's checks that covers it.
    pub fn check_format(&self) -> Option<&'static str> {
        doctor::check_format(&self.codec, self.bit_depth, self.chroma_log2)
    }

    fn describe(&self) -> String {
        let depth = self
            .bit_depth
            .map_or(String::new(), |d| format!(" {d}-bit"));
        let chroma = match self.chroma_log2 {
            Some((1, 1)) => " 4:2:0",
            Some((1, 0)) => " 4:2:2",
            Some((0, 0)) => " 4:4:4",
            _ => "",
        };
        format!("{}{depth}{chroma}", self.codec)
    }
}

/// The decoder chosen for a stream, and why, for the snapshot and the
/// overlay.
#[derive(Debug, Clone)]
pub struct DecoderChoice {
    pub mode: DecodeMode,
    pub why: String,
}

/// The decoder for a stream of `format` (`None` when it isn't known),
/// by `pref`, with `support` from doctor and `discrete` saying whether
/// the GPU that renders, which decodes too, is a discrete one.
pub fn choose(
    pref: DecoderPreference,
    support: Option<&HwSupport>,
    format: Option<&StreamFormat>,
    discrete: bool,
) -> DecoderChoice {
    let hardware = |kind, fallback| DecodeMode::Hardware {
        kind,
        device: None,
        fallback,
    };
    let software = |why: String| DecoderChoice {
        mode: DecodeMode::Software,
        why,
    };
    match pref {
        DecoderPreference::Software => software("software, as set".into()),
        DecoderPreference::Only(kind) => DecoderChoice {
            mode: hardware(kind, Fallback::Fail),
            why: format!("{kind} only, as set"),
        },
        DecoderPreference::Auto => {
            let Some(format) = format else {
                return software("software: the stream's format isn't known".into());
            };
            let Some(name) = format.check_format() else {
                return software(format!(
                    "software: doctor checks no hardware decoding of {}",
                    format.describe()
                ));
            };
            let Some(support) = support else {
                return software("software: doctor's checks haven't run".into());
            };
            let works = |k| support.works(name, k);
            let kind = match (works(HwKind::VaApi), works(HwKind::Vulkan)) {
                (false, false) => {
                    let why = |k| {
                        support
                            .get(name, k)
                            .and_then(|c| c.reason.clone())
                            .unwrap_or_else(|| "not checked".into())
                    };
                    return software(format!(
                        "software: {name} decodes on no device (VA-API: {}; Vulkan: {})",
                        why(HwKind::VaApi),
                        why(HwKind::Vulkan)
                    ));
                }
                (true, false) => (HwKind::VaApi, format!("{name} decodes on VA-API only")),
                (false, true) => (HwKind::Vulkan, format!("{name} decodes on Vulkan only")),
                (true, true) if discrete => (
                    HwKind::Vulkan,
                    format!(
                        "{name} decodes on both, and the GPU is discrete, where VA-API's copy \
                         out of video memory is the slower"
                    ),
                ),
                (true, true) => (
                    HwKind::VaApi,
                    format!("{name} decodes on both, and the GPU isn't a discrete one"),
                ),
            };
            DecoderChoice {
                mode: hardware(kind.0, Fallback::Software),
                why: format!("{}: {}", kind.0, kind.1),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support(checks: &[(&str, HwKind, bool)]) -> HwSupport {
        HwSupport {
            checks: checks
                .iter()
                .map(|&(format, kind, works)| HwDecodeCheck {
                    format: format.into(),
                    kind,
                    result: if works {
                        CheckResult::Works
                    } else {
                        CheckResult::Fails
                    },
                    reason: (!works).then(|| "Device does not support decoding hevc!".into()),
                })
                .collect(),
        }
    }

    fn hevc(depth: u32) -> StreamFormat {
        StreamFormat {
            codec: "hevc".into(),
            bit_depth: Some(depth),
            chroma_log2: Some((1, 1)),
        }
    }

    fn kind_of(c: &DecoderChoice) -> Option<(HwKind, Fallback)> {
        match &c.mode {
            DecodeMode::Software => None,
            DecodeMode::Hardware { kind, fallback, .. } => Some((*kind, *fallback)),
        }
    }

    #[test]
    fn auto_picks_the_kind_doctor_found_working() {
        use HwKind::*;
        let auto = |s: &HwSupport, f: &StreamFormat, discrete| {
            kind_of(&choose(DecoderPreference::Auto, Some(s), Some(f), discrete))
        };
        // huginn on stock Mesa: VA-API decodes HEVC, Vulkan doesn't.
        let huginn = support(&[("HEVC Main", VaApi, true), ("HEVC Main", Vulkan, false)]);
        assert_eq!(
            auto(&huginn, &hevc(8), false),
            Some((VaApi, Fallback::Software))
        );
        // Only Vulkan works: Vulkan, not VA-API first.
        let vulkan = support(&[("HEVC Main", VaApi, false), ("HEVC Main", Vulkan, true)]);
        assert_eq!(
            auto(&vulkan, &hevc(8), false),
            Some((Vulkan, Fallback::Software))
        );
        // Both: the discrete GPU takes Vulkan, an integrated one VA-API.
        let both = support(&[("HEVC Main", VaApi, true), ("HEVC Main", Vulkan, true)]);
        assert_eq!(
            auto(&both, &hevc(8), true),
            Some((Vulkan, Fallback::Software))
        );
        assert_eq!(
            auto(&both, &hevc(8), false),
            Some((VaApi, Fallback::Software))
        );
        // Per format: Main works, Main 10 wasn't checked here.
        assert_eq!(auto(&both, &hevc(10), true), None);
        // Neither works: software, with doctor's reasons.
        let none = support(&[("HEVC Main", VaApi, false), ("HEVC Main", Vulkan, false)]);
        let c = choose(DecoderPreference::Auto, Some(&none), Some(&hevc(8)), true);
        assert_eq!(kind_of(&c), None);
        assert!(
            c.why.contains("Device does not support decoding hevc!"),
            "{}",
            c.why
        );
    }

    #[test]
    fn auto_decodes_in_software_what_no_check_covers() {
        let both = support(&[("HEVC Main 10", HwKind::VaApi, true)]);
        let prores = StreamFormat {
            codec: "prores".into(),
            bit_depth: Some(10),
            chroma_log2: Some((1, 0)),
        };
        let c = choose(DecoderPreference::Auto, Some(&both), Some(&prores), false);
        assert_eq!(kind_of(&c), None);
        assert!(c.why.contains("prores 10-bit 4:2:2"), "{}", c.why);
        let c = choose(DecoderPreference::Auto, Some(&both), None, false);
        assert_eq!(kind_of(&c), None);
        let c = choose(DecoderPreference::Auto, None, Some(&hevc(10)), false);
        assert_eq!(kind_of(&c), None);
    }

    #[test]
    fn set_preferences_are_kept() {
        let none = HwSupport::default();
        let c = choose(
            DecoderPreference::Software,
            Some(&none),
            Some(&hevc(8)),
            true,
        );
        assert_eq!(kind_of(&c), None);
        let c = choose(
            DecoderPreference::Only(HwKind::VaApi),
            Some(&none),
            None,
            true,
        );
        assert_eq!(kind_of(&c), Some((HwKind::VaApi, Fallback::Fail)));
    }

    #[test]
    fn a_promise_gives_the_checks_kept_or_none_once_broken_or_late() {
        let support = support(&[("HEVC Main", HwKind::VaApi, true)]);
        let (keeper, promise) = HwPromise::new();
        let waiter = {
            let promise = promise.clone();
            std::thread::spawn(move || promise.wait(Duration::from_secs(60)))
        };
        std::thread::sleep(Duration::from_millis(50));
        keeper.keep(support.clone());
        assert_eq!(waiter.join().unwrap(), Some(support.clone()));
        assert_eq!(promise.wait(Duration::ZERO), Some(support));

        // The keeper dropped (its thread panicked): the wait ends at once.
        let (keeper, promise) = HwPromise::new();
        let t0 = Instant::now();
        let waiter = std::thread::spawn(move || promise.wait(Duration::from_secs(60)));
        drop(keeper);
        assert_eq!(waiter.join().unwrap(), None);
        assert!(t0.elapsed() < Duration::from_secs(30));

        let (_keeper, promise) = HwPromise::new();
        assert_eq!(promise.wait(Duration::from_millis(20)), None);
    }
}
