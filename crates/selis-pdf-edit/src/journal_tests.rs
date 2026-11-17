#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]
use super::journal::*;

// The journal's own property tests (SL-5.EDIT.01 DoD).

use super::*;

fn bytes(text: &str) -> Bytes {
    Bytes::copy_from_slice(text.as_bytes())
}

fn change(before: &str, after: &str, op: &str) -> Mutation {
    Mutation::ReplaceDocument {
        op: op.to_string(),
        before: bytes(before),
        after: bytes(after),
    }
}

fn text(value: &Bytes) -> &str {
    std::str::from_utf8(value).unwrap_or("")
}

#[test]
fn materialise_is_the_source_when_empty_and_the_last_change_otherwise() {
    let source = bytes("one");
    let mut journal = Journal::new(&source);
    assert_eq!(
        text(&journal.materialise(&source).expect("materialise")),
        "one"
    );
    journal.apply(
        change("one", "two", "edit_text"),
        "Two",
        1,
        ActorId::local(),
        Coalesce::Never,
    );
    assert_eq!(
        text(&journal.materialise(&source).expect("materialise")),
        "two"
    );
    journal.apply(
        change("two", "three", "redact"),
        "Three",
        2,
        ActorId::local(),
        Coalesce::Never,
    );
    assert_eq!(
        text(&journal.materialise(&source).expect("materialise")),
        "three"
    );
}

#[test]
fn applying_then_reverting_any_sequence_returns_the_original() {
    let source = bytes("zero");
    let mut journal = Journal::new(&source);
    let steps = ["one", "two", "three", "four"];
    let mut current = "zero";
    for (index, next) in steps.iter().enumerate() {
        journal.apply(
            change(current, next, "edit_text"),
            *next,
            index as Nanos,
            ActorId::local(),
            Coalesce::Never,
        );
        current = next;
    }
    for index in (0..steps.len()).rev() {
        let expected = if index == 0 { "zero" } else { steps[index - 1] };
        assert_eq!(text(&journal.undo(&source).expect("undo")), expected);
    }
    assert!(!journal.can_undo());
}

#[test]
fn the_inverse_swaps_the_states() {
    let mutation = change("a", "b", "rotate");
    let inverse = mutation.inverse();
    match inverse {
        Mutation::ReplaceDocument { before, after, op } => {
            assert_eq!(text(&before), "b");
            assert_eq!(text(&after), "a");
            assert_eq!(op, "rotate");
        }
    }
    assert!(mutation.reversible());
}

#[test]
fn undo_is_one_group_not_one_entry() {
    let source = bytes("start");
    let mut journal = Journal::new(&source);
    for (at, (from, to)) in [("start", "a"), ("a", "b"), ("b", "c")].iter().enumerate() {
        journal.apply(
            change(from, to, "edit_text"),
            *to,
            at as Nanos,
            ActorId::local(),
            Coalesce::Never,
        );
    }
    assert_eq!(journal.groups.len(), 3);
    assert_eq!(text(&journal.undo(&source).expect("undo")), "b");
    assert_eq!(text(&journal.undo(&source).expect("undo")), "a");
    assert_eq!(text(&journal.undo(&source).expect("undo")), "start");
}

#[test]
fn typing_coalesces_within_the_idle_window() {
    let source = bytes("");
    let mut journal = Journal::new(&source);
    let rule = Coalesce::Typing { idle: 500 };
    journal.apply(
        change("", "h", "edit_text"),
        "h",
        1000,
        ActorId::local(),
        rule,
    );
    journal.apply(
        change("h", "he", "edit_text"),
        "he",
        1100,
        ActorId::local(),
        rule,
    );
    journal.apply(
        change("he", "hel", "edit_text"),
        "hel",
        1601,
        ActorId::local(),
        rule,
    );
    assert_eq!(journal.groups.len(), 2);
    assert_eq!(journal.groups[0].entries, 0..2);
}

#[test]
fn a_new_change_discards_the_redo_branch() {
    let source = bytes("one");
    let mut journal = Journal::new(&source);
    journal.apply(
        change("one", "two", "edit_text"),
        "Two",
        1,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.apply(
        change("two", "three", "redact"),
        "Three",
        2,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.undo(&source).expect("undo");
    assert!(journal.can_redo());
    journal.apply(
        change("two", "other", "stamp"),
        "Other",
        3,
        ActorId::local(),
        Coalesce::Never,
    );
    assert!(!journal.can_redo());
    assert!(!journal.entries.iter().any(|entry| entry.label == "Three"));
}

#[test]
fn redo_reapplies_a_group_oldest_first() {
    let source = bytes("one");
    let mut journal = Journal::new(&source);
    journal.apply(
        change("one", "two", "edit_text"),
        "Two",
        1,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.apply(
        change("two", "three", "redact"),
        "Three",
        2,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.undo(&source).expect("undo");
    assert_eq!(text(&journal.redo(&source).expect("redo")), "three");
}

#[test]
fn a_bundle_reloads_to_the_same_state() {
    let source = bytes("one");
    let mut journal = Journal::new(&source);
    journal.apply(
        change("one", "two", "edit_text"),
        "Two",
        1,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.apply(
        change("two", "three", "redact"),
        "Three",
        2,
        ActorId::local(),
        Coalesce::Never,
    );
    journal.undo(&source).expect("undo");
    let bundle = journal.to_bundle();
    let reloaded = Journal::from_bundle(&bundle, &source).expect("reload");
    assert_eq!(reloaded.position(), journal.position());
    assert_eq!(
        reloaded.materialise(&source).expect("materialise"),
        journal.materialise(&source).expect("materialise")
    );
    assert_eq!(reloaded.entries, journal.entries);
    assert_eq!(reloaded.groups, journal.groups);
}

#[test]
fn a_changed_source_is_a_hard_error() {
    let source = bytes("one");
    let mut journal = Journal::new(&source);
    journal.apply(
        change("one", "two", "edit_text"),
        "Two",
        1,
        ActorId::local(),
        Coalesce::Never,
    );
    let bundle = journal.to_bundle();
    assert_eq!(
        Journal::from_bundle(&bundle, &bytes("different")).unwrap_err(),
        BundleError::SourceChanged
    );
    assert_eq!(
        journal.materialise(&bytes("different")).unwrap_err(),
        SourceChanged
    );
}

#[test]
fn the_fingerprint_catches_a_same_size_edit() {
    assert_eq!(fingerprint(&bytes("one")), fingerprint(&bytes("one")));
    assert_ne!(fingerprint(&bytes("one")), fingerprint(&bytes("two")));
    assert_eq!(fingerprint(&bytes("one")).size, 3);
}
