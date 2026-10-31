//! The product name lives here and nowhere else. User-visible strings, the
//! application id and config/cache directory names are all derived from
//! these constants, so renaming the product is a one-file change (plus the
//! crate and binary names, which are a mechanical search and replace).

/// Human-readable product name.
pub const APP_NAME: &str = "Spherecut";

/// Lower-case name used for the binary, config and cache directories.
pub const APP_SLUG: &str = slug!();

/// [`APP_SLUG`] as a literal, for `concat!`.
macro_rules! slug {
    () => {
        "spherecut"
    };
}
pub(crate) use slug;

/// Reverse-DNS application id (Wayland app_id, .desktop file name).
pub const APP_ID: &str = "io.github.cyclonesurfer.Spherecut";

/// Version of the running build.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
