//! SL-4.UI.06: the four navigation reads, against a fixture that carries all
//! four.
//!
//! The viewer reads a document's **bookmarks** (`/Outlines`), its **page
//! labels** (`/PageLabels`), its **named destinations** (`/Dests`) and one
//! page's **link annotations** (`/Annots`). They are four different structures
//! in four different places in the file, and the failure mode of each is the
//! same: the reader is told something false about their own document.
//!
//! - A document with no outline must not look like a host that cannot read
//!   one.
//! - A page's own label ("iv") is not its number ("4"), and a viewer that
//!   reports the number is quietly wrong.
//! - A bookmark that NAMES a destination is navigable, and only once the name
//!   has been resolved against the document's name tree.
//! - A link's action class crosses verbatim - including the classes ADR-0020
//!   disables - because the engine reports and the viewer decides.
//!
//! The fixture is `corpus/fixtures/tagged_structure.pdf`, written by
//! `tools/make-tagged-fixture.mjs`. It has TWO pages on purpose: a bookmark
//! that points at page 1 of a one-page document cannot be shown to work,
//! because navigating to page 1 and doing nothing look identical.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use selis_pdf_doc::{ActionKind, NavTarget};
use selis_pdf_engine::Session;
use selis_sandbox::{Budget, CancelToken, FixedClock, Surface};

/// The navigation fixture. Selis-authored and committed, so this does not
/// depend on the fetched-corpus cache. Bytes are embedded at compile time:
/// L0-L3 crates must stay filesystem-free (xtask check-purity), even in their
/// tests.
static TAGGED: &[u8] = include_bytes!("../../../corpus/fixtures/tagged_structure.pdf");

fn budget() -> Budget {
	Budget::profile(Surface::Viewer)
}

fn session() -> Session {
	let b = budget();
	Session::open(TAGGED.to_vec(), &b, &FixedClock(0)).expect("Session::open")
}

/// The page number a target names, if it names one.
fn target_page(target: &NavTarget) -> Option<u32> {
	match target {
		NavTarget::Page(page) => Some(page.page),
		_ => None,
	}
}

#[test]
fn the_bookmark_tree_is_present_and_nested() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let tree = session.outline(&b, &mut g).expect("outline");

	assert!(tree.present, "the fixture declares an /Outlines tree");
	assert!(!tree.truncated, "the fixture's tree is complete");
	assert_eq!(tree.items.len(), 2, "two top-level chapters");
	assert_eq!(tree.items[0].title, "Chapter One");
	assert_eq!(tree.items[1].title, "Chapter Two");
	assert_eq!(
		tree.items[1].children.len(),
		2,
		"the second chapter has two children"
	);
}

#[test]
fn a_bookmark_points_at_a_page_and_a_named_one_does_not() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let tree = session.outline(&b, &mut g).expect("outline");

	// "Chapter Two" points at PAGE 2 by a plain page reference.
	let chapter_two = &tree.items[1];
	assert_eq!(
		target_page(chapter_two.target.as_ref().expect("a target")),
		Some(1),
		"page 2 is page index 1"
	);

	// The last child names a DESTINATION. It is not a page target, and the
	// engine passes that through rather than resolving it: the viewer is the
	// layer that chases the chain.
	let named = &chapter_two.children[1];
	assert_eq!(named.title, "Section 2.2 via action");
	match named.target.as_ref().expect("a target") {
		NavTarget::Named(name) => assert_eq!(name, "chapter-two"),
		other => panic!("expected a named target, got {other:?}"),
	}
}

#[test]
fn the_page_labels_are_the_documents_own() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let labels = session.page_labels(&b, &mut g).expect("page labels");

	assert_eq!(labels.len(), 2, "two label ranges");
	assert_eq!(labels[0].page_index, 0);
	assert_eq!(
		labels[0].style.as_deref(),
		Some(&b"r"[..]),
		"the front matter is lower-roman"
	);
	assert_eq!(labels[1].page_index, 1);
	assert_eq!(labels[1].style.as_deref(), Some(&b"D"[..]), "the body is decimal");
}

#[test]
fn the_named_destination_resolves_to_a_page() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let destinations = session.named_destinations(&b, &mut g).expect("named destinations");

	assert_eq!(destinations.len(), 1, "the fixture names one destination");
	assert_eq!(destinations[0].name, "chapter-two");
	assert_eq!(
		target_page(&destinations[0].target),
		Some(1),
		"the name points at page 2"
	);
}

#[test]
fn the_links_are_read_with_their_action_classes_verbatim() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let links = session.page_links(0, &b, &mut g).expect("page links");

	assert_eq!(links.len(), 3, "three link annotations on page 1");

	// The external one.
	let uri = &links[0];
	assert_eq!(uri.action.as_ref().expect("an action").kind, ActionKind::Uri);
	assert_eq!(
		uri.action.as_ref().expect("an action").uri.as_deref(),
		Some("https://example.com/selis?ref=fixture"),
		"the URI is verbatim, query string and all"
	);
	assert_eq!(uri.contents.as_deref(), Some("Read more"));
	assert!(
		uri.rect.is_some(),
		"a link with a rectangle can be placed; one without cannot"
	);

	// The internal one.
	let internal = &links[1];
	assert_eq!(
		internal.action.as_ref().expect("an action").kind,
		ActionKind::GoTo
	);
	assert_eq!(
		target_page(internal.target.as_ref().expect("a target")),
		Some(1),
		"it points at page 2"
	);

	// The one ADR-0020 disables. It crosses anyway: the engine reports, the
	// viewer decides, and a transport that dropped it would make "is the
	// viewer refusing this?" unanswerable from the outside.
	let launch = &links[2];
	assert_eq!(
		launch.action.as_ref().expect("an action").kind,
		ActionKind::Launch
	);
}

#[test]
fn a_page_with_no_links_yields_an_empty_list_and_not_an_error() {
	let b = budget();
	let session = session();
	let mut g = b.guard_with(&FixedClock(0), CancelToken::new());
	let links = session.page_links(1, &b, &mut g).expect("page links");
	assert!(
		links.is_empty(),
		"page 2 carries no annotations; an empty list is the answer, not a failure"
	);
}
