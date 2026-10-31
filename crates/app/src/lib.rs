//! Spherecut's GUI (M1 plan section 7): eframe on the device render
//! opens, the tiles, the viewer's paint callback, grab and zoom, the
//! projection switch, transport, the panes and the notices.
//!
//! The UI thread never blocks (section 8.6): `clippy.toml` here bans the
//! blocking calls, the few that run on workers or around the event loop
//! say why, and `core::blocking` panics on the marked thread in tests.

#![forbid(unsafe_code)]

pub mod app;
pub mod doc;
pub mod jobs;
pub mod notices;
pub mod overlay;
pub mod panes;
pub mod proto;
pub mod reframe;
pub mod run;
pub mod settings;
pub mod smoke;
pub mod timeline;
pub mod transport;
pub mod view;
pub mod viewer;

pub use app::{AppParts, AppState, DoctorRun, Pane, SpherecutApp};
pub use run::{GuiArgs, GuiError, install, run};
pub use settings::Settings;
