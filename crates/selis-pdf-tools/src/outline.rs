//! Outline (bookmark) editing: replace the document's outline tree with a
//! caller-supplied list of titled page destinations.
//!
//! One rewrite covers add, rename, retarget, reorder, and delete: the caller
//! sends the complete list it wants, and the engine builds a fresh tree from
//! it. Entries carry a nesting `level` (0 = top level), so a reader shows the
//! same hierarchy the caller described.
//!
//! The old outline tree is dropped through
//! [`DocumentBuilder::retain_reachable`], so a replaced bookmark does not
//! linger as unreachable bytes in the output.

use selis_bytes::Bytes;
use selis_pdf_cos::doc_writer::DocumentBuilder;
use selis_pdf_cos::{Obj, Ref};
use selis_sandbox::Budget;
use serde::{Deserialize, Serialize};

use crate::verify_report::Verification;
use crate::{read_file, CliError, CliResult, ToolResult};

/// One outline entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineEntry {
    /// The display title. Must not be empty after trimming.
    pub title: String,
    /// The 1-based physical page the entry points at.
    pub page: u32,
    /// The nesting depth: 0 is a top-level bookmark.
    #[serde(default)]
    pub level: u32,
}

/// Options for [`set_outline`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OutlineOptions {
    /// The complete outline, in reading order. An empty list removes every
    /// bookmark.
    pub entries: Vec<OutlineEntry>,
}

/// The most entries one rewrite accepts. A hostile request must not turn into
/// an unbounded object graph.
const MAX_OUTLINE_ENTRIES: usize = 10_000;

/// Replace the document's outline with `options.entries`.
///
/// # Errors
///
/// A typed [`CliError`] when an entry has an empty title, a zero or
/// out-of-range page, a nesting jump of more than one level, the list exceeds
/// [`MAX_OUTLINE_ENTRIES`], the input is encrypted or unreadable, or the
/// rewritten output fails structural verification.
pub fn set_outline(path: &str, options: &OutlineOptions, output: &str) -> ToolResult<Verification> {
    if options.entries.len() > MAX_OUTLINE_ENTRIES {
        return Err(CliError(format!(
            "an outline may hold at most {MAX_OUTLINE_ENTRIES} entries (got {})",
            options.entries.len()
        )));
    }
    let mut previous_level: Option<u32> = None;
    for (index, entry) in options.entries.iter().enumerate() {
        let number = index.saturating_add(1);
        if entry.title.trim().is_empty() {
            return Err(CliError(format!(
                "outline entry {number} has an empty title"
            )));
        }
        if entry.page == 0 {
            return Err(CliError(format!(
                "outline entry {number} targets page 0; pages are 1-based"
            )));
        }
        match previous_level {
            None if entry.level != 0 => {
                return Err(CliError(
                    "the first outline entry must be at level 0".to_string(),
                ));
            }
            Some(previous) if entry.level > previous.saturating_add(1) => {
                return Err(CliError(format!(
                    "outline entry {number} jumps from level {previous} to {}; nesting may only grow one level at a time",
                    entry.level
                )));
            }
            _ => {}
        }
        previous_level = Some(entry.level);
    }

    let src = read_file(path)?;
    let in_bytes = u64::try_from(src.len()).unwrap_or(u64::MAX);
    let budget = Budget::unlimited();
    let clock = crate::shell_clock();
    let mut g = crate::runtime::cli_guard(&budget, &clock);
    let startxref = selis_pdf_cos::xref::find_startxref(&src, 4096).unwrap_or(0);
    let doc = selis_pdf_cos::parse_revisions(&src, startxref, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: cannot open: {e}")))?;
    crate::tools::refuse_encrypted_input(path, &doc)?;
    let page_refs = crate::tools::page_refs_of(&src, &doc, &budget, &mut g)
        .map_err(|e| CliError(format!("{path}: {e}")))?;
    if page_refs.is_empty() {
        return Err(CliError(format!("{path}: document has no pages")));
    }
    let page_count = u32::try_from(page_refs.len()).unwrap_or(u32::MAX);
    for (index, entry) in options.entries.iter().enumerate() {
        if entry.page > page_count {
            return Err(CliError(format!(
                "outline entry {} targets page {} but the document has {page_count}",
                index.saturating_add(1),
                entry.page
            )));
        }
    }

    let mut out = DocumentBuilder::new();
    let mut next_num = 3u32;
    let mut kept: Vec<crate::tools::KeptPage> = Vec::with_capacity(page_refs.len());
    for (index, page_ref) in page_refs.iter().enumerate() {
        let merged_ref =
            crate::tools::copy_page(&mut out, &src, *page_ref, &budget, &mut g, &mut next_num)
                .map_err(|e| CliError(format!("{path}: {e}")))?;
        kept.push((index, page_ref.num, merged_ref));
    }
    crate::tools::reconcile_single_input(
        &mut out, &src, &doc, &page_refs, &kept, true, &budget, &mut g,
    )
    .map_err(CliError)?;

    if options.entries.is_empty() {
        out.add_catalog_entry(b"Outlines", Obj::Null);
    } else {
        let root = build_outline(&mut out, options, &kept)?;
        out.add_catalog_entry(b"Outlines", Obj::Ref(root));
    }
    // The reconcile pass copied the input's outline tree before we replaced
    // the catalog entry; drop it now that nothing can reach it.
    out.retain_reachable();

    let entry_count = u64::try_from(options.entries.len()).unwrap_or(u64::MAX);
    let verification = crate::tools::write_document_with_outline(
        out,
        output,
        Some(&src),
        crate::write_gate::PRESERVE_ALL,
        Some(u64::try_from(kept.len()).unwrap_or(u64::MAX)),
        Some(entry_count),
        in_bytes,
        &budget,
        &mut g,
    )?;
    eprintln!("wrote {entry_count} outline entr(y/ies) -> {output}");
    verification.emit_line();
    Ok(verification)
}

/// Build the outline objects and return the root's reference.
///
/// One outline item under construction: its own object number and the links a
/// reader needs to walk the tree. Links hold resolved object numbers, so the
/// writer pass never has to index the list again.
#[derive(Debug, Default)]
struct OutlineItem {
    num: u32,
    /// The parent's object number; `None` means the outline root is the parent.
    parent: Option<u32>,
    previous: Option<u32>,
    next: Option<u32>,
    first_child: Option<u32>,
    last_child: Option<u32>,
    /// How many items sit below this one, at every depth.
    descendants: usize,
}

/// The object number of `index`, or a typed error when the list lost an item.
fn num_of(items: &[OutlineItem], index: usize) -> CliResult<u32> {
    items.get(index).map(|item| item.num).ok_or_else(|| {
        CliError(format!(
            "outline item {} has no object number",
            index.saturating_add(1)
        ))
    })
}

/// Linking and descendant counts are resolved in single passes over the list:
/// a 10 000-entry request must not cost the caller a quadratic loop on the
/// main thread.
fn build_outline(
    out: &mut DocumentBuilder,
    options: &OutlineOptions,
    kept: &[crate::tools::KeptPage],
) -> CliResult<Ref> {
    let entries = &options.entries;
    let mut items: Vec<OutlineItem> = Vec::with_capacity(entries.len());
    for _ in entries {
        items.push(OutlineItem {
            num: out.allocate(),
            ..OutlineItem::default()
        });
    }
    let root_num = out.allocate();
    let root = Ref::new(root_num, 0);

    let mut first_top: Option<u32> = None;
    let mut last_top: Option<u32> = None;
    // `last_at[level]` is the entry index of the most recent item at that
    // level; the next deeper item becomes its child.
    let mut last_at: Vec<usize> = Vec::new();

    for (index, entry) in entries.iter().enumerate() {
        let level = usize::try_from(entry.level).unwrap_or(usize::MAX);
        let parent_index = if level == 0 {
            None
        } else {
            Some(*last_at.get(level.saturating_sub(1)).ok_or_else(|| {
                CliError(format!(
                    "outline entry {} has no parent at level {}",
                    index.saturating_add(1),
                    level.saturating_sub(1)
                ))
            })?)
        };
        let previous_index = last_at.get(level).copied();
        let this_num = num_of(&items, index)?;
        let parent_num = parent_index
            .map(|value| num_of(&items, value))
            .transpose()?;
        let previous_num = previous_index
            .map(|value| num_of(&items, value))
            .transpose()?;

        if let Some(item) = items.get_mut(index) {
            item.parent = parent_num;
            item.previous = previous_num;
        }
        // Close the sibling chain in both directions.
        if let Some(previous) = previous_index {
            if let Some(sibling) = items.get_mut(previous) {
                sibling.next = Some(this_num);
            }
        }
        // Link into the parent's child chain, or the root's for a top item.
        if let Some(parent) = parent_index {
            if let Some(node) = items.get_mut(parent) {
                node.first_child.get_or_insert(this_num);
                node.last_child = Some(this_num);
            }
        } else {
            first_top.get_or_insert(this_num);
            last_top = Some(this_num);
        }

        last_at.truncate(level);
        last_at.push(index);
    }

    // A subtree closes when a following entry returns to its level or above,
    // so one stack of open ancestors yields every descendant count.
    let mut open: Vec<usize> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        while open.last().is_some_and(|&top| {
            entries
                .get(top)
                .is_some_and(|ancestor| ancestor.level >= entry.level)
        }) {
            close_subtree(&mut open, &mut items);
        }
        open.push(index);
    }
    while !open.is_empty() {
        close_subtree(&mut open, &mut items);
    }

    let kept_page = |page: u32| -> CliResult<Ref> {
        let position = usize::try_from(page)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .ok_or_else(|| CliError("page index overflow".to_string()))?;
        kept.get(position)
            .map(|(_, _, merged)| *merged)
            .ok_or_else(|| CliError(format!("page {page} was not copied")))
    };

    for (entry, item) in entries.iter().zip(items.iter()) {
        let page_ref = kept_page(entry.page)?;
        let mut dict: Vec<(Bytes, Obj)> = Vec::new();
        dict.push((bytes(b"Title"), pdf_text(&entry.title)));
        dict.push((
            bytes(b"Parent"),
            Obj::Ref(Ref::new(item.parent.unwrap_or(root_num), 0)),
        ));
        dict.push((
            bytes(b"Dest"),
            Obj::Array(vec![Obj::Ref(page_ref), Obj::Name(bytes(b"Fit"))]),
        ));
        if let Some(previous) = item.previous {
            dict.push((bytes(b"Prev"), Obj::Ref(Ref::new(previous, 0))));
        }
        if let Some(next) = item.next {
            dict.push((bytes(b"Next"), Obj::Ref(Ref::new(next, 0))));
        }
        if let Some(first) = item.first_child {
            dict.push((bytes(b"First"), Obj::Ref(Ref::new(first, 0))));
            if let Some(last) = item.last_child {
                dict.push((bytes(b"Last"), Obj::Ref(Ref::new(last, 0))));
            }
            dict.push((
                bytes(b"Count"),
                Obj::Int(i64::try_from(item.descendants).unwrap_or(i64::MAX)),
            ));
        }
        out.add_object(item.num, Obj::Dict(dict));
    }

    let mut root_dict: Vec<(Bytes, Obj)> = Vec::new();
    root_dict.push((bytes(b"Type"), Obj::Name(bytes(b"Outlines"))));
    if let (Some(first), Some(last)) = (first_top, last_top) {
        root_dict.push((bytes(b"First"), Obj::Ref(Ref::new(first, 0))));
        root_dict.push((bytes(b"Last"), Obj::Ref(Ref::new(last, 0))));
    }
    root_dict.push((
        bytes(b"Count"),
        Obj::Int(i64::try_from(entries.len()).unwrap_or(i64::MAX)),
    ));
    out.add_object(root_num, Obj::Dict(root_dict));
    Ok(root)
}

/// Pop the closed frame and add its whole subtree to the frame now topping the
/// stack, which is its parent.
fn close_subtree(open: &mut Vec<usize>, items: &mut [OutlineItem]) {
    let Some(top) = open.pop() else { return };
    let Some(&parent) = open.last() else { return };
    let subtree = match items.get(top) {
        Some(item) => item.descendants.saturating_add(1),
        None => return,
    };
    if let Some(node) = items.get_mut(parent) {
        node.descendants = node.descendants.saturating_add(subtree);
    }
}

/// A PDF text string: ASCII stays literal, anything else becomes UTF-16BE with
/// a byte-order mark so the reader does not have to guess.
fn pdf_text(text: &str) -> Obj {
    if text.is_ascii() {
        return Obj::String(Bytes::copy_from_slice(text.as_bytes()));
    }
    let mut bytes = vec![0xfe, 0xff];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    Obj::String(Bytes::copy_from_slice(&bytes))
}

fn bytes(value: &[u8]) -> Bytes {
    Bytes::copy_from_slice(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use selis_sandbox::Budget;

    fn three_page_pdf() -> Vec<u8> {
        let budget = Budget::unlimited();
        let mut g = budget.guard();
        let mut builder = DocumentBuilder::new();
        for _ in 0..3 {
            builder.add_page(595.28, 841.89, b"q Q");
        }
        builder.write(&budget, &mut g).expect("fixture")
    }

    fn entry(title: &str, page: u32, level: u32) -> OutlineEntry {
        OutlineEntry {
            title: title.to_string(),
            page,
            level,
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("selis-outline-{name}"));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn nested_entries_are_written_and_verified() {
        let dir = temp_dir("nested");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");

        let verification = set_outline(
            input.to_str().unwrap(),
            &OutlineOptions {
                entries: vec![
                    entry("Chapter 1", 1, 0),
                    entry("Section 1.1", 2, 1),
                    entry("Chapter 2", 3, 0),
                ],
            },
            output.to_str().unwrap(),
        )
        .expect("set outline");

        assert_eq!(verification.outline_entries_in, Some(0));
        assert_eq!(verification.outline_entries_out, 3);
        assert_eq!(verification.pages_out, 3);
        assert!(verification.checked.contains(&"outline_entries"));

        let text = String::from_utf8_lossy(&std::fs::read(&output).expect("read")).to_string();
        assert!(text.contains("/Type /Outlines"));
        assert!(text.contains("(Chapter 1)"));
        assert!(text.contains("(Section 1.1)"));
        assert!(text.contains("(Chapter 2)"));
        assert!(text.contains("/First"));
        assert!(text.contains("/Count 3"));
    }

    #[test]
    fn a_second_rewrite_replaces_the_old_tree() {
        let dir = temp_dir("replace");
        let input = dir.join("in.pdf");
        let first = dir.join("first.pdf");
        let second = dir.join("second.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");

        set_outline(
            input.to_str().unwrap(),
            &OutlineOptions {
                entries: vec![entry("Old title", 1, 0), entry("Old child", 2, 1)],
            },
            first.to_str().unwrap(),
        )
        .expect("first outline");

        let verification = set_outline(
            first.to_str().unwrap(),
            &OutlineOptions {
                entries: vec![entry("New title", 3, 0)],
            },
            second.to_str().unwrap(),
        )
        .expect("second outline");

        assert_eq!(verification.outline_entries_in, Some(2));
        assert_eq!(verification.outline_entries_out, 1);
        let text = String::from_utf8_lossy(&std::fs::read(&second).expect("read")).to_string();
        assert!(text.contains("(New title)"));
        assert!(!text.contains("(Old title)"), "the replaced title is gone");
        assert!(!text.contains("(Old child)"));
    }

    /// A three-level tree: the gate walks First/Next/Parent to count entries,
    /// so a count of 5 means every link resolved, and each `/Count` is the
    /// size of that item's whole subtree.
    #[test]
    fn a_nested_chain_counts_every_descendant() {
        let dir = temp_dir("chain");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");

        let verification = set_outline(
            input.to_str().unwrap(),
            &OutlineOptions {
                entries: vec![
                    entry("Chapter 1", 1, 0),
                    entry("Section 1.1", 1, 1),
                    entry("Clause 1.1.1", 1, 2),
                    entry("Section 1.2", 2, 1),
                    entry("Chapter 2", 3, 0),
                ],
            },
            output.to_str().unwrap(),
        )
        .expect("nested outline");

        assert_eq!(verification.outline_entries_out, 5);
        assert_eq!(verification.pages_out, 3);

        let text = String::from_utf8_lossy(&std::fs::read(&output).expect("read")).to_string();
        assert!(text.contains("/Count 5"), "the root counts all five");
        assert!(text.contains("/Count 3"), "Chapter 1 holds three items");
        assert!(text.contains("/Count 1"), "Section 1.1 holds one clause");
    }

    #[test]
    fn an_empty_list_removes_every_bookmark() {
        let dir = temp_dir("empty");
        let input = dir.join("in.pdf");
        let first = dir.join("first.pdf");
        let cleared = dir.join("cleared.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");

        set_outline(
            input.to_str().unwrap(),
            &OutlineOptions {
                entries: vec![entry("Only", 1, 0)],
            },
            first.to_str().unwrap(),
        )
        .expect("first outline");
        let verification = set_outline(
            first.to_str().unwrap(),
            &OutlineOptions::default(),
            cleared.to_str().unwrap(),
        )
        .expect("clear outline");

        assert_eq!(verification.outline_entries_in, Some(1));
        assert_eq!(verification.outline_entries_out, 0);
        let text = String::from_utf8_lossy(&std::fs::read(&cleared).expect("read")).to_string();
        assert!(!text.contains("(Only)"));
    }

    #[test]
    fn invalid_entries_are_refused_before_any_write() {
        let dir = temp_dir("invalid");
        let input = dir.join("in.pdf");
        let output = dir.join("out.pdf");
        std::fs::write(&input, three_page_pdf()).expect("write input");
        let path = input.to_str().unwrap();
        let out = output.to_str().unwrap();

        for (entries, expected) in [
            (vec![entry("  ", 1, 0)], "empty title"),
            (vec![entry("Zero", 0, 0)], "1-based"),
            (vec![entry("Past", 4, 0)], "document has 3"),
            (vec![entry("Jump", 1, 2)], "level 0"),
            (
                vec![entry("A", 1, 0), entry("B", 2, 0), entry("C", 3, 2)],
                "one level at a time",
            ),
        ] {
            let error = set_outline(path, &OutlineOptions { entries }, out).expect_err("refused");
            assert!(
                error.0.contains(expected),
                "error {:?} did not mention {expected:?}",
                error.0
            );
        }
        assert!(!output.exists(), "a refused edit writes nothing");
    }
}
