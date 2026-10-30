//! The notice banners (M1 plan section 7.8): a stack under the menu bar,
//! one per kind, each dismissible, with the commands that fix it and a
//! button that copies them.

use spherecut_media::doctor::{Issue, Severity};

/// What a notice is about. A second notice of a kind replaces the first,
/// and stays dismissed if the first was.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoticeKind {
    /// Hardware decoding went on in software, for a stream.
    Fallback(usize),
    NoAudio,
    NoAudioDevice,
    AudioFailed,
    CpuAdapter,
    NotEquirect,
    /// A camera's raw file, its lenses side by side.
    Lenses,
    Stereo,
    Hdr,
    Pose,
    ColorAssumed,
    CacheTooSmall,
    OpenFailed(usize),
    DecodeFailed(usize),
    GpuFailed,
    SlotDropped,
    UploadFailed,
    /// One of the doctor's issues, by what it says.
    Doctor(String),
}

/// One banner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub kind: NoticeKind,
    pub severity: Severity,
    pub what: String,
    pub impact: String,
    /// Commands or a sentence, one a line; empty when nothing fixes it.
    pub fix: String,
    pub dismissed: bool,
}

impl Notice {
    pub fn new(kind: NoticeKind, severity: Severity, what: impl Into<String>) -> Notice {
        Notice {
            kind,
            severity,
            what: what.into(),
            impact: String::new(),
            fix: String::new(),
            dismissed: false,
        }
    }

    pub fn impact(mut self, impact: impl Into<String>) -> Notice {
        self.impact = impact.into();
        self
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Notice {
        self.fix = fix.into();
        self
    }

    /// The same notice as another kind: an issue of the app's own.
    pub fn with_kind(mut self, kind: NoticeKind) -> Notice {
        self.kind = kind;
        self
    }

    /// A doctor's issue as a banner.
    pub fn from_issue(issue: &Issue) -> Notice {
        Notice::new(
            NoticeKind::Doctor(issue.what.clone()),
            issue.severity,
            issue.what.clone(),
        )
        .impact(issue.impact.clone())
        .fix(issue.fix.clone())
    }

    /// The text the copy button copies: what, impact and the fix.
    pub fn copy_text(&self) -> String {
        let mut s = self.what.clone();
        if !self.impact.is_empty() {
            s.push_str("\nimpact: ");
            s.push_str(&self.impact);
        }
        if !self.fix.is_empty() {
            s.push_str("\nfix:\n");
            for line in self.fix.lines() {
                s.push_str("  ");
                s.push_str(line.trim());
                s.push('\n');
            }
        }
        s
    }
}

/// The stack.
#[derive(Debug, Clone, Default)]
pub struct Notices {
    items: Vec<Notice>,
}

impl Notices {
    /// Show `notice`, in place of any of its kind.
    pub fn push(&mut self, mut notice: Notice) {
        if let Some(old) = self.items.iter_mut().find(|n| n.kind == notice.kind) {
            notice.dismissed = old.dismissed;
            *old = notice;
        } else {
            self.items.push(notice);
        }
    }

    /// Take down the notices of kinds `drop` says, such as a file's when
    /// another opens.
    pub fn retain(&mut self, keep: impl Fn(&NoticeKind) -> bool) {
        self.items.retain(|n| keep(&n.kind));
    }

    pub fn dismiss(&mut self, kind: &NoticeKind) {
        if let Some(n) = self.items.iter_mut().find(|n| n.kind == *kind) {
            n.dismissed = true;
        }
    }

    /// The banners up, worst first, then oldest.
    pub fn shown(&self) -> Vec<&Notice> {
        let mut v: Vec<&Notice> = self.items.iter().filter(|n| !n.dismissed).collect();
        v.sort_by_key(|n| n.severity);
        v
    }

    pub fn all(&self) -> &[Notice] {
        &self.items
    }

    pub fn get(&self, kind: &NoticeKind) -> Option<&Notice> {
        self.items.iter().find(|n| n.kind == *kind)
    }

    /// Draw the banners up. A copy puts the notice's text on the
    /// clipboard (eframe's `clipboard` feature).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut dismiss = Vec::new();
        for n in self.shown() {
            let (fill, label) = match n.severity {
                Severity::Error => (ui.visuals().error_fg_color, "Error"),
                Severity::Warning => (ui.visuals().warn_fg_color, "Warning"),
                Severity::Info => (ui.visuals().weak_text_color(), "Note"),
            };
            egui::Frame::group(ui.style())
                .stroke(egui::Stroke::new(1.0, fill))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(fill, label);
                        ui.label(egui::RichText::new(&n.what).strong());
                        if !n.impact.is_empty() {
                            ui.label(&n.impact);
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("Dismiss")
                                .on_hover_text("Dismiss")
                                .clicked()
                            {
                                dismiss.push(n.kind.clone());
                            }
                            if ui
                                .small_button("Copy")
                                .on_hover_text("Copy what this says and the fix")
                                .clicked()
                            {
                                ui.ctx().copy_text(n.copy_text());
                            }
                        });
                    });
                    if !n.fix.is_empty() {
                        for line in n.fix.lines() {
                            ui.monospace(line.trim());
                        }
                    }
                });
        }
        for k in dismiss {
            self.dismiss(&k);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_shows_once_stays_dismissed_and_sorts_by_severity() {
        let mut n = Notices::default();
        n.push(Notice::new(NoticeKind::NoAudio, Severity::Info, "no audio"));
        n.push(Notice::new(
            NoticeKind::CpuAdapter,
            Severity::Warning,
            "cpu",
        ));
        n.push(Notice::new(
            NoticeKind::NoAudio,
            Severity::Info,
            "no audio again",
        ));
        assert_eq!(n.all().len(), 2);
        assert_eq!(n.shown()[0].kind, NoticeKind::CpuAdapter);
        assert_eq!(n.get(&NoticeKind::NoAudio).unwrap().what, "no audio again");
        n.dismiss(&NoticeKind::NoAudio);
        n.push(Notice::new(
            NoticeKind::NoAudio,
            Severity::Info,
            "and again",
        ));
        assert_eq!(n.shown().len(), 1);
        n.retain(|k| *k != NoticeKind::NoAudio);
        assert!(n.get(&NoticeKind::NoAudio).is_none());
    }

    #[test]
    fn copy_text_has_the_fix_one_command_a_line() {
        let n = Notice::new(NoticeKind::CpuAdapter, Severity::Warning, "what")
            .impact("slow")
            .fix("sudo dnf install a\n   sudo dnf install b");
        assert_eq!(
            n.copy_text(),
            "what\nimpact: slow\nfix:\n  sudo dnf install a\n  sudo dnf install b\n"
        );
    }
}
