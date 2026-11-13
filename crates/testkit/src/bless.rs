//! Whether a run blesses: one rule for every variable that turns a
//! blessing on (the project review's F055). [`UPDATE_GOLDENS`] blesses the
//! render goldens, the pipeline goldens and the instrument's floors;
//! [`BLESS_PROJECT`] blesses core's schema 2 fixture. Unset, empty and "0"
//! are off, and any other value is on; [`LOWER`] is on, and also lets a
//! blessing lower a floor by more than its drop.
//!
//! `cargo xtask check` and `cargo xtask hw-test` refuse to start while
//! either is on ([`on_in`]), since a run of either would rewrite what the
//! tests hold the code to.

use std::ffi::{OsStr, OsString};

/// Blesses the render goldens, the pipeline goldens and the instrument's
/// floors.
pub const UPDATE_GOLDENS: &str = "SPHERECUT_UPDATE_GOLDENS";

/// Blesses core's schema 2 fixture, `schema2.spherecut`.
pub const BLESS_PROJECT: &str = "SPHERECUT_BLESS_PROJECT";

/// Every variable that turns a blessing on.
pub const VARIABLES: [&str; 2] = [UPDATE_GOLDENS, BLESS_PROJECT];

/// The value that also lets a blessing lower a floor by more than its
/// drop.
pub const LOWER: &str = "lower";

/// Whether a blessing variable's `value` turns it on: off unset, empty or
/// "0", on otherwise.
pub fn is_on(value: Option<&OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether a blessing variable's `value` is [`LOWER`].
pub fn is_lower(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new(LOWER))
}

/// Whether the variable `name` is on in this process's environment.
pub fn on(name: &str) -> bool {
    is_on(std::env::var_os(name).as_deref())
}

/// Whether the variable `name` is [`LOWER`] in this process's environment.
pub fn lower(name: &str) -> bool {
    is_lower(std::env::var_os(name).as_deref())
}

/// Those of [`VARIABLES`] that are on, as `var` looks a variable up (the
/// environment's `std::env::var_os`, or a test's own).
pub fn on_in(var: &dyn Fn(&str) -> Option<OsString>) -> Vec<&'static str> {
    VARIABLES
        .into_iter()
        .filter(|name| is_on(var(name).as_deref()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::must_fail;

    /// How `rule` reads each value that must be off and each that must be
    /// on: the first it reads wrongly, if any.
    fn reads_values(rule: impl Fn(Option<&OsStr>) -> bool) -> Result<(), String> {
        let off: [Option<&str>; 3] = [None, Some(""), Some("0")];
        let on = ["1", "yes", LOWER, "00", " 0"];
        for v in off {
            if rule(v.map(OsStr::new)) {
                return Err(format!("{v:?} read as on"));
            }
        }
        for v in on {
            if !rule(Some(OsStr::new(v))) {
                return Err(format!("{v:?} read as off"));
            }
        }
        Ok(())
    }

    #[test]
    fn unset_empty_and_zero_are_off_and_the_rest_on() {
        reads_values(is_on).unwrap();
        // Control: the rule by presence, as two of the four blessings
        // read their variable before F055, blesses at "0".
        let e = must_fail("the rule by presence", reads_values(|v| v.is_some()));
        assert_eq!(e, "Some(\"\") read as on");
        let e = must_fail(
            "the rule by presence, past the empty value",
            reads_values(|v| v.is_some_and(|v| !v.is_empty())),
        );
        assert_eq!(e, "Some(\"0\") read as on");
    }

    #[test]
    fn lower_is_its_own_value() {
        assert!(is_lower(Some(OsStr::new(LOWER))));
        for v in [None, Some(""), Some("0"), Some("1"), Some("LOWER")] {
            assert!(!is_lower(v.map(OsStr::new)), "{v:?}");
        }
    }

    #[test]
    fn each_variable_on_is_named() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(on_in(&env(&[])), Vec::<&str>::new());
        assert_eq!(
            on_in(&env(&[(UPDATE_GOLDENS, "0"), (BLESS_PROJECT, "")])),
            Vec::<&str>::new()
        );
        assert_eq!(on_in(&env(&[(BLESS_PROJECT, "1")])), [BLESS_PROJECT]);
        assert_eq!(
            on_in(&env(&[(UPDATE_GOLDENS, LOWER), (BLESS_PROJECT, "1")])),
            VARIABLES
        );
    }
}
