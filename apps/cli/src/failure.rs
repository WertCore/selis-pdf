//! Honest failure messaging (SL-1A.UI.06): budget exhaustion and
//! cancellation are reported with which budget tripped, the measured usage,
//! and what to do about it — never a raw panic path, never a bare code name.
//!
//! Classification reads the engine's typed error rendering: the Display of a
//! `selis_error::Error` starts with the registry id, `[E<n>]`, and ids never
//! change meaning (03-CONVENTIONS.md §3). The budget exhaustion detail is
//! engine-controlled text of the shape `bytes limit=N requested=M`
//! (`Resource::as_str` + two numbers, ADR-P0017) — parsed defensively here,
//! with the raw message always available as the fallback.

/// The failure classes the CLI words differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The user's Ctrl-C, observed by the engine (typed `CANCELLED`).
    Cancelled,
    /// A budget dimension was exhausted (alloc/objects/depth/wall/pixels),
    /// with the limit and the requested total when the detail carried them.
    Budget {
        /// `alloc` | `objects` | `depth` | `wall` | `pixels`.
        resource: &'static str,
        /// The budget's limit.
        limit: Option<u64>,
        /// The running total that would have been needed.
        requested: Option<u64>,
    },
    /// Everything else: the message is already honest.
    Other,
}

/// Classify an error message by its leading registry id.
#[must_use]
pub(crate) fn classify(message: &str) -> Failure {
    let id = code_id(message);
    match id {
        Some(4020) => Failure::Cancelled,
        Some(4000) => Failure::Budget {
            resource: "alloc",
            limit: None,
            requested: None,
        },
        Some(4001) => Failure::Budget {
            resource: "wall",
            limit: None,
            requested: None,
        },
        Some(4002) => Failure::Budget {
            resource: "depth",
            limit: None,
            requested: None,
        },
        Some(4003) => Failure::Budget {
            resource: "objects",
            limit: None,
            requested: None,
        },
        Some(4004) => Failure::Budget {
            resource: "pixels",
            limit: None,
            requested: None,
        },
        // A poisoned guard means a prior exhaustion already stopped this
        // operation; the detail names the resource that exhausted it.
        Some(4010) => Failure::Budget {
            resource: poisoned_resource(message),
            limit: None,
            requested: None,
        },
        _ => Failure::Other,
    }
    .with_usage(message)
}

/// The resource word a `BUDGET_POISONED` detail carries (the resource that
/// exhausted the guard, rendered as its own `(...)` segment). Unknown words
/// degrade to the generic budget name.
fn poisoned_resource(message: &str) -> &'static str {
    for word in ["bytes", "objects", "depth", "wall", "pixels"] {
        if message.contains(&format!("(detail {word})")) || message.contains(&format!("({word})")) {
            return match word {
                "bytes" => "alloc",
                other => other,
            };
        }
    }
    "budget"
}

impl Failure {
    /// Attach the measured usage from the error's detail text
    /// (`<resource> limit=<n> requested=<m>`), when present.
    fn with_usage(self, message: &str) -> Self {
        match self {
            Failure::Budget { resource, .. } => {
                let (limit, requested) = parse_usage(message);
                Failure::Budget {
                    resource,
                    limit,
                    requested,
                }
            }
            other => other,
        }
    }
}

/// The first `[E<id>]` registry id in an error rendering, if any. Map-err
/// wrappers prefix context (`"{path}: {e}"`), so the marker is matched
/// anywhere in the message; the ids themselves never change meaning
/// (03-CONVENTIONS.md §3).
fn code_id(message: &str) -> Option<u32> {
    let start = message.find("[E")?;
    let rest = message.get(start.saturating_add(2)..)?;
    let end = rest.find(']')?;
    rest.get(..end)?.parse().ok()
}

/// Parse `limit=<n>` / `requested=<m>` out of the message's detail.
fn parse_usage(message: &str) -> (Option<u64>, Option<u64>) {
    (
        parse_number_after(message, "limit="),
        parse_number_after(message, "requested="),
    )
}

/// The unsigned integer immediately following `marker`, if any.
fn parse_number_after(message: &str, marker: &str) -> Option<u64> {
    let rest = message.split(marker).nth(1)?;
    let token = rest
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())?;
    token.parse().ok()
}

/// The process exit code for a failure: 130 is the conventional
/// "terminated by SIGINT" code, used here for the clean cooperative
/// cancellation so shell scripts can tell the two apart.
#[must_use]
pub(crate) fn exit_code(failure: &Failure) -> i32 {
    match failure {
        Failure::Cancelled => 130,
        Failure::Budget { .. } | Failure::Other => 1,
    }
}

/// What to do about an exhausted budget, per dimension. Plain and honest:
/// the one remedy that actually works for a file too big for its budget.
#[must_use]
pub(crate) fn remedy(resource: &str) -> &'static str {
    match resource {
        "alloc" => "this file needs more memory than the operation's budget allows; split it into smaller files and process the parts",
        "objects" => "this file has more objects than the operation's budget allows; split it into smaller files and process the parts",
        "depth" => "this document nests deeper than the budget allows; split it into smaller files, or run `selis inspect` to find the pathological structure",
        "wall" => "this file takes longer than the operation's deadline allows; split it into smaller files and process the parts",
        _ => "this file exceeds the operation's budget; split it into smaller files and process the parts",
    }
}

/// The full stderr text for a failed command.
#[must_use]
pub(crate) fn message(raw: &str, failure: &Failure) -> String {
    match failure {
        Failure::Cancelled => format!(
            "selis: cancelled — no partial file was written; the destination is \
             unchanged or holds the last complete verified output\n  ({raw})"
        ),
        Failure::Budget {
            resource,
            limit,
            requested,
        } => {
            let usage = match (limit, requested) {
                (Some(l), Some(r)) => format!("limit {l}, needed {r}"),
                (Some(l), None) => format!("limit {l}"),
                _ => String::new(),
            };
            let usage_line = if usage.is_empty() {
                format!("selis: budget exceeded ({resource})")
            } else {
                format!("selis: budget exceeded ({resource}): {usage}")
            };
            format!("{usage_line}\n  {}\n  ({raw})", remedy(resource))
        }
        Failure::Other => format!("selis: error: {raw}"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    /// Budget errors classify by registry id and carry the measured usage.
    #[test]
    fn budget_errors_classify_with_usage() {
        // The Display of a BUDGET_BYTES error: [E4000] + user message +
        // detail "bytes limit=… requested=…".
        let raw = "[E4000] This document needs more memory than Selis is allowed to use here. \
                   (during budget-charge) (detail bytes limit=268435456 requested=268435456)";
        match classify(raw) {
            Failure::Budget {
                resource,
                limit,
                requested,
            } => {
                assert_eq!(resource, "alloc");
                assert_eq!(limit, Some(268435456));
                assert_eq!(requested, Some(268435456));
            }
            other => panic!("expected budget, got {other:?}"),
        }
    }

    /// Every budget dimension maps to its stable short name, and a poisoned
    /// guard or unknown message falls back to `Other`.
    #[test]
    fn each_dimension_and_fallbacks() {
        for (id, name) in [
            (4000u32, "alloc"),
            (4001, "wall"),
            (4002, "depth"),
            (4003, "objects"),
            (4004, "pixels"),
        ] {
            let raw = format!("[E{id}] user text (detail x limit=1 requested=2)");
            match classify(&raw) {
                Failure::Budget { resource, .. } => assert_eq!(resource, name),
                other => panic!("expected budget {name}, got {other:?}"),
            }
        }
        assert_eq!(classify("[E4020] Cancelled."), Failure::Cancelled);
        assert_eq!(classify("plain CLI mistake"), Failure::Other);
        // A poisoned guard is the prior exhaustion: a budget failure whose
        // resource comes from the detail's resource word.
        assert_eq!(
            classify("[E4010] already stopped (during budget-charge) (depth)"),
            Failure::Budget {
                resource: "depth",
                limit: None,
                requested: None
            }
        );
        assert_eq!(
            classify("[E4010] already stopped"),
            Failure::Budget {
                resource: "budget",
                limit: None,
                requested: None
            }
        );
    }

    /// Usage parsing is defensive: missing or malformed details degrade to
    /// `None`, never a panic.
    #[test]
    fn usage_parsing_is_defensive() {
        assert_eq!(parse_usage("no detail here"), (None, None));
        assert_eq!(parse_usage("detail limit=42"), (Some(42), None));
        assert_eq!(parse_usage("detail requested=7"), (None, Some(7)));
        assert_eq!(parse_usage("limit=not-a-number"), (None, None));
        // The marker is found through map-err wrappers, not only at the head.
        assert_eq!(code_id("cannot open PDF: [E4003] x"), Some(4003));
        assert_eq!(code_id("[E4003] x"), Some(4003));
        assert_eq!(code_id("no id"), None);
    }

    /// The cancel message says what the user needs to know: nothing partial
    /// was written.
    #[test]
    fn cancel_message_promises_no_partial_file() {
        let msg = message(
            "[E4020] Cancelled. (during budget-charge)",
            &classify("[E4020] Cancelled."),
        );
        assert!(msg.contains("cancelled"), "{msg}");
        assert!(msg.contains("no partial file"), "{msg}");
        assert_eq!(exit_code(&Failure::Cancelled), 130);
        assert_eq!(exit_code(&Failure::Other), 1);
    }

    /// The budget message names the dimension, the measured usage, and the
    /// remedy.
    #[test]
    fn budget_message_names_dimension_usage_and_remedy() {
        let raw = "[E4002] nests too deeply (detail depth limit=64 requested=65)";
        let failure = classify(raw);
        let msg = message(raw, &failure);
        assert!(msg.contains("budget exceeded (depth)"), "{msg}");
        assert!(msg.contains("limit 64, needed 65"), "{msg}");
        assert!(msg.contains("split it into smaller files"), "{msg}");
        assert_eq!(exit_code(&failure), 1);
    }
}
