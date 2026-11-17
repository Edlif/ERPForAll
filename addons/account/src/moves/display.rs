// SPDX-License-Identifier: MIT OR Apache-2.0
//! The display name of an entry (breadcrumb, many2one labels, the form heading): a draft says what
//! kind of document it is, an entry with a number shows the number, and the reference follows in
//! brackets.
//! The labels are English only: the server has no translation of code strings yet.

use erp_sdk::prelude::*;

use super::AccountMove;

/// The longest reference shown in a display name, in characters, before it is cut at a word.
const REF_WIDTH: usize = 50;
/// What marks a shortened reference.
const CUT_MARK: &str = " [...]";

#[methods(AccountMove, addon = "account")]
impl AccountMove {
    /// The display name of each entry, from its state, kind, number and reference.
    #[compute(depends = ["name", "ref", "state", "move_type"])]
    async fn _compute_display_name(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMove>,
    ) -> Result<()> {
        let names = this.mapped(tx, AccountMove::NAME).await?;
        let refs = this.mapped(tx, AccountMove::REF).await?;
        let states = this.mapped(tx, AccountMove::STATE).await?;
        let kinds = this.mapped(tx, AccountMove::MOVE_TYPE).await?;
        for (index, (((name, reference), state), kind)) in names
            .into_iter()
            .zip(refs)
            .zip(states)
            .zip(kinds)
            .enumerate()
        {
            let label = display_name(
                name.as_deref(),
                reference.as_deref(),
                state.as_deref(),
                kind.as_deref(),
            )?;
            out.set(
                index,
                AccountMove::DISPLAY_NAME,
                label.map(Arc::<str>::from),
            )?;
        }
        Ok(())
    }
}

/// What a draft of `kind` is called before it has a number.
fn draft_label(kind: Option<&str>) -> Result<&'static str> {
    Ok(match kind {
        Some("entry") => "Draft Entry",
        Some("out_invoice") => "Draft Invoice",
        Some("out_refund") => "Draft Credit Note",
        Some("in_invoice") => "Draft Bill",
        Some("in_refund") => "Draft Vendor Credit Note",
        other => {
            return Err(Error::programming(format!(
                "account.move: no display name for the entry type {other:?}"
            )));
        }
    })
}

/// The display name of one entry; `None` when it has neither a draft label nor a number.
fn display_name(
    name: Option<&str>,
    reference: Option<&str>,
    state: Option<&str>,
    kind: Option<&str>,
) -> Result<Option<String>> {
    let mut label = if state == Some("draft") {
        draft_label(kind)?.to_owned()
    } else {
        String::new()
    };
    if let Some(number) = name.filter(|n| !n.is_empty() && *n != "/") {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push_str(number);
    }
    if let Some(reference) = reference.map(shorten).filter(|r| !r.is_empty()) {
        if !label.is_empty() {
            label.push(' ');
        }
        label.push('(');
        label.push_str(&reference);
        label.push(')');
    }
    Ok((!label.is_empty()).then_some(label))
}

/// `text` with its whitespace collapsed, cut at a word so it fits [`REF_WIDTH`] characters with
/// the [`CUT_MARK`].
fn shorten(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let whole = words.join(" ");
    if whole.chars().count() <= REF_WIDTH {
        return whole;
    }
    let room = REF_WIDTH - CUT_MARK.chars().count();
    let mut kept = String::new();
    for word in words {
        let extra = usize::from(!kept.is_empty()) + word.chars().count();
        if kept.chars().count() + extra > room {
            break;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(word);
    }
    kept.push_str(CUT_MARK);
    kept
}

#[cfg(test)]
mod tests {
    use super::{display_name, shorten};

    fn name(n: Option<&str>, r: Option<&str>, s: &str, k: &str) -> Option<String> {
        display_name(n, r, Some(s), Some(k)).unwrap()
    }

    #[test]
    fn a_draft_is_named_by_its_kind_and_a_numbered_entry_by_its_number() {
        assert_eq!(
            name(None, None, "draft", "out_invoice").as_deref(),
            Some("Draft Invoice")
        );
        assert_eq!(
            name(None, None, "draft", "in_invoice").as_deref(),
            Some("Draft Bill")
        );
        assert_eq!(
            name(None, None, "draft", "out_refund").as_deref(),
            Some("Draft Credit Note")
        );
        assert_eq!(
            name(None, None, "draft", "in_refund").as_deref(),
            Some("Draft Vendor Credit Note")
        );
        assert_eq!(
            name(Some("/"), None, "draft", "entry").as_deref(),
            Some("Draft Entry")
        );
        // a draft that was posted once keeps its number next to the kind
        assert_eq!(
            name(Some("INV/2024/0001"), None, "draft", "out_invoice").as_deref(),
            Some("Draft Invoice INV/2024/0001")
        );
        assert_eq!(
            name(Some("INV/2024/0001"), Some("PO 7"), "posted", "out_invoice").as_deref(),
            Some("INV/2024/0001 (PO 7)")
        );
        assert_eq!(name(None, None, "cancel", "entry"), None);
        assert!(display_name(None, None, Some("draft"), Some("receipt")).is_err());
        assert!(display_name(None, None, Some("draft"), None).is_err());
    }

    #[test]
    fn a_long_reference_is_cut_at_a_word() {
        assert_eq!(shorten("  a   b "), "a b");
        let long = "word ".repeat(20);
        let cut = shorten(&long);
        assert!(cut.chars().count() <= 50, "{cut}");
        assert!(cut.ends_with(" [...]"), "{cut}");
        assert!(cut.starts_with("word word"), "{cut}");
    }
}
