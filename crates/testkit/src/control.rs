//! Controls: every check is also run where it must fail (M3 plan 4.1
//! rule 1, DECISIONS D32).
//!
//! A check that can't fail proves nothing. So beside each check a suite
//! runs the same check on input that is wrong on purpose (the lenses
//! swapped, a centre a pixel off, a term switched off) and hands what the
//! check came to to [`must_fail`]. A control that passes fails the test,
//! naming the control: a control that stops failing fails the test. One
//! that fails gives back how it failed, so the caller can check that it
//! failed for the right reason, and by enough.
//!
//! A check comes in one of three shapes, each an [`Outcome`]: a `Result`,
//! whose failure is its error; a `bool`; or a [`Comparison`] of a number
//! with its limit ([`at_most`], [`at_least`], [`below`] and [`above`]),
//! whose failure is the number. Compare numbers through a [`Comparison`]
//! rather than as a `bool`: `x <= limit` is false for a NaN, which would
//! make a broken measurement look like a control failing as it should,
//! while a comparison with a NaN in it decides nothing and fails the test.

use std::fmt::{self, Debug, Display};

/// What a check came to, as [`must_fail`] reads it.
pub trait Outcome {
    /// What a check that failed gives back: its error, or the value that
    /// failed.
    type Failure;

    /// How the check failed, or, when it didn't, why not, in words.
    fn into_failure(self) -> Result<Self::Failure, String>;
}

/// A check that returns a `Result` fails with its error.
impl<T: Debug, E> Outcome for Result<T, E> {
    type Failure = E;

    fn into_failure(self) -> Result<E, String> {
        match self {
            Err(e) => Ok(e),
            Ok(v) => Err(format!("it passed, with {v:?}")),
        }
    }
}

/// A check that returns a `bool` fails with `false`.
impl Outcome for bool {
    type Failure = ();

    fn into_failure(self) -> Result<(), String> {
        if self {
            Err("it passed".to_owned())
        } else {
            Ok(())
        }
    }
}

/// A number held to a limit, made by [`at_most`], [`at_least`], [`below`]
/// or [`above`]. It passes when the number keeps to the limit, fails when
/// it doesn't, and decides nothing when the two don't compare (a NaN).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Comparison<T> {
    value: T,
    limit: T,
    rule: Rule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rule {
    AtMost,
    AtLeast,
    Below,
    Above,
}

/// `value` held to at most `limit`.
pub fn at_most<T>(value: T, limit: T) -> Comparison<T> {
    Comparison {
        value,
        limit,
        rule: Rule::AtMost,
    }
}

/// `value` held to at least `limit`.
pub fn at_least<T>(value: T, limit: T) -> Comparison<T> {
    Comparison {
        value,
        limit,
        rule: Rule::AtLeast,
    }
}

/// `value` held to less than `limit`.
pub fn below<T>(value: T, limit: T) -> Comparison<T> {
    Comparison {
        value,
        limit,
        rule: Rule::Below,
    }
}

/// `value` held to more than `limit`.
pub fn above<T>(value: T, limit: T) -> Comparison<T> {
    Comparison {
        value,
        limit,
        rule: Rule::Above,
    }
}

impl<T: PartialOrd> Comparison<T> {
    /// Whether the value keeps to its limit, or `None` when the two don't
    /// compare.
    pub fn passes(&self) -> Option<bool> {
        let o = self.value.partial_cmp(&self.limit)?;
        Some(match self.rule {
            Rule::AtMost => o.is_le(),
            Rule::AtLeast => o.is_ge(),
            Rule::Below => o.is_lt(),
            Rule::Above => o.is_gt(),
        })
    }
}

/// As the check reads, such as `31.8 ≥ 36.0`.
impl<T: Debug> Display for Comparison<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = match self.rule {
            Rule::AtMost => "≤",
            Rule::AtLeast => "≥",
            Rule::Below => "<",
            Rule::Above => ">",
        };
        write!(f, "{:?} {sign} {:?}", self.value, self.limit)
    }
}

/// A comparison fails with its value.
impl<T: PartialOrd + Debug> Outcome for Comparison<T> {
    type Failure = T;

    fn into_failure(self) -> Result<T, String> {
        match self.passes() {
            Some(false) => Ok(self.value),
            Some(true) => Err(format!("it passed: {self}")),
            None => Err(format!("{self} decides nothing: the two don't compare")),
        }
    }
}

/// The failure of control `name`, which must fail; a control that passes,
/// or decides nothing, fails the test with a message naming it.
///
/// `outcome` is what the control's check came to: a `Result` gives back
/// its error, a `bool` nothing, and a [`Comparison`] its value, so the
/// caller can check that the control failed for the right reason, and by
/// enough.
///
/// ```
/// use spherecut_testkit::control::{at_least, must_fail};
///
/// // The check: a render holds 36 dB against its reference.
/// let holds = |psnr_db: f64| at_least(psnr_db, 36.0);
/// assert_eq!(holds(41.3).passes(), Some(true));
///
/// // Its control: with a lens a pixel off, the same check must fail, and
/// // by 3 dB or more.
/// let off = must_fail("lens 1 a pixel off", holds(31.8));
/// assert!(36.0 - off >= 3.0);
///
/// // A check that returns a Result gives its error back, so the control
/// // can say it failed for the right reason.
/// let e = must_fail("a number past u8", "300".parse::<u8>());
/// assert_eq!(e.kind(), &std::num::IntErrorKind::PosOverflow);
///
/// // And a bool.
/// let even = |n: u32| n % 2 == 0;
/// must_fail("7 taken for even", even(7));
/// ```
#[track_caller]
pub fn must_fail<O: Outcome>(name: &str, outcome: O) -> O::Failure {
    match outcome.into_failure() {
        Ok(failure) => failure,
        Err(why) => panic!(
            "control `{name}` did not fail ({why}): a control that stops failing fails the \
             test (DECISIONS D32)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `f` panicked with, or `None` if it returned.
    fn panic_message(f: impl FnOnce() + std::panic::UnwindSafe) -> Option<String> {
        std::panic::catch_unwind(f).err().map(|e| {
            e.downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default()
        })
    }

    #[test]
    fn a_failing_result_gives_its_error_back() {
        let e = must_fail("wrong binding", Err::<(), _>("tilt 2.3° across the seam"));
        assert_eq!(e, "tilt 2.3° across the seam");
    }

    #[test]
    fn a_passing_result_fails_the_test_naming_the_control() {
        let msg = panic_message(|| {
            must_fail("wrong binding", Ok::<_, String>(0.4));
        })
        .unwrap();
        assert_eq!(
            msg,
            "control `wrong binding` did not fail (it passed, with 0.4): a control that stops \
             failing fails the test (DECISIONS D32)"
        );
    }

    #[test]
    fn a_bool_fails_with_false() {
        must_fail("swapped lenses match", false);
        let msg = panic_message(|| must_fail("swapped lenses match", true)).unwrap();
        assert!(
            msg.starts_with("control `swapped lenses match` did not fail (it passed)"),
            "{msg}"
        );
    }

    #[test]
    fn a_comparison_that_fails_gives_its_value_back() {
        assert_eq!(must_fail("psnr", at_least(31.8, 36.0)), 31.8);
        assert_eq!(must_fail("bad pixels", at_most(140_usize, 100)), 140);
        assert_eq!(must_fail("margin", above(3.0, 3.0)), 3.0);
        assert_eq!(must_fail("residual", below(1e-9, 1e-9)), 1e-9);
    }

    #[test]
    fn a_comparison_that_passes_fails_the_test() {
        for (c, shown) in [
            (at_least(41.3, 36.0), "41.3 ≥ 36.0"),
            (at_most(36.0, 36.0), "36.0 ≤ 36.0"),
            (below(2.0, 3.0), "2.0 < 3.0"),
            (above(4.0, 3.0), "4.0 > 3.0"),
        ] {
            assert_eq!(c.passes(), Some(true));
            let msg = panic_message(|| {
                must_fail("lens 1 a pixel off", c);
            })
            .unwrap();
            assert!(
                msg.starts_with(&format!(
                    "control `lens 1 a pixel off` did not fail (it passed: {shown})"
                )),
                "{msg}"
            );
        }
    }

    #[test]
    fn a_nan_decides_nothing() {
        // As a bool, NaN ≥ 36 is false: a broken measurement would look
        // like a control failing. As a comparison it fails the test.
        let nan = f64::NAN;
        must_fail("NaN as a bool", nan >= 36.0);
        assert_eq!(at_least(nan, 36.0).passes(), None);
        let msg = panic_message(|| {
            must_fail("psnr", at_least(nan, 36.0));
        })
        .unwrap();
        assert!(
            msg.starts_with("control `psnr` did not fail (NaN ≥ 36.0 decides nothing"),
            "{msg}"
        );
    }
}
