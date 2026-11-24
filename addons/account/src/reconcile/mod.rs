// SPDX-License-Identifier: MIT OR Apache-2.0
//! Reconciliation of receivable and payable journal items, first cut (with amendment A7,
//! /D5; reference `account_partial_reconcile.py`, `account_full_reconcile.py` and the
//! reconcile part of `account_move_line.py`, observed for behaviour only).
//!
//! A match between a debit item and a credit item is an [`AccountPartialReconcile`]; the items a
//! chain of matches connects form a component labelled by its smallest match id (`P<id>` on every
//! member). When every member of a component is settled, one [`AccountFullReconcile`] groups them
//! and becomes their matching number. What is still due on a matched item lives in an
//! [`AccountOpenItem`] row, changed by the amount of each match (never recomputed from the other
//! matches) and bounded by database CHECKs, so an over-applied match fails whatever path wrote it.
//!
//! Built in this cut: posted items in the company's currency on accounts that allow reconciliation,
//! partial and full matching, the residual and matching number of an item, the amount due and
//! payment status of an invoice, and unmatching. Refused by name until later slices: items in
//! another currency, items of drafts, write-offs, cash-basis taxes and early-payment discounts.
//! Open-item rows are created when an item is first matched (an unmatched item reads its own
//! amounts), not yet by the line triggers. Developer page.

mod apply;
mod coverage;
mod guards;
pub mod history_guard;
mod lines;
pub(crate) mod plan;
mod removal;

pub use apply::{reconcile_lines, unreconcile_partials};

use erp_sdk::prelude::*;

/// The group whose members may match and unmatch journal items (the posting group).
pub use crate::moves::POSTER_GROUP as RECONCILER_GROUP;

/// A match of part or all of a debit item with a credit item.
///
/// Rows are written only by the reconciliation service ([`reconcile_lines`]); deleting one goes
/// through [`unreconcile_partials`]. Every other ORM change is refused, for a superuser too
/// (`guards.rs`).
#[model(
    name = "account.partial.reconcile",
    description = "Partial Reconcile",
    order = "id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        (
            "_partial_amount_bounds",
            "CHECK (amount >= 0 AND debit_amount_currency >= 0 AND credit_amount_currency >= 0 \
             AND (amount > 0 OR debit_amount_currency > 0 OR credit_amount_currency > 0))",
            "A match carries a positive amount."
        ),
        (
            "_partial_distinct_endpoints",
            "CHECK (debit_move_id <> credit_move_id)",
            "A journal item cannot be matched with itself."
        ),
    ],
)]
pub struct AccountPartialReconcile {
    /// The debit item.
    #[field(
        many2one,
        comodel = "account.move.line",
        required = true,
        ondelete = "restrict",
        readonly = true,
        string = "Debit Item"
    )]
    pub debit_move_id: Option<RecId>,
    /// The credit item.
    #[field(
        many2one,
        comodel = "account.move.line",
        required = true,
        ondelete = "restrict",
        readonly = true,
        string = "Credit Item"
    )]
    pub credit_move_id: Option<RecId>,
    /// The settlement both items belong to once every member of their component is settled.
    #[field(
        many2one,
        comodel = "account.full.reconcile",
        ondelete = "set null",
        readonly = true,
        index = "btree_not_null",
        string = "Full Reconcile"
    )]
    pub full_reconcile_id: Option<RecId>,
    /// The debit item's company.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        readonly = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The company currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        string = "Company Currency"
    )]
    pub company_currency_id: Option<RecId>,
    /// Currency of the debit item.
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        readonly = true,
        string = "Debit Currency"
    )]
    pub debit_currency_id: Option<RecId>,
    /// Currency of the credit item.
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        readonly = true,
        string = "Credit Currency"
    )]
    pub credit_currency_id: Option<RecId>,
    /// Matched amount in the company currency.
    #[field(
        monetary,
        currency_field = "company_currency_id",
        readonly = true,
        string = "Amount"
    )]
    pub amount: Decimal,
    /// Matched amount in the debit item's currency.
    #[field(
        monetary,
        currency_field = "debit_currency_id",
        readonly = true,
        string = "Debit Amount in Currency"
    )]
    pub debit_amount_currency: Decimal,
    /// Matched amount in the credit item's currency.
    #[field(
        monetary,
        currency_field = "credit_currency_id",
        readonly = true,
        string = "Credit Amount in Currency"
    )]
    pub credit_amount_currency: Decimal,
    /// The later date of the two items.
    #[field(date, readonly = true, string = "Later Item Date")]
    pub max_date: Option<Date>,
}

/// The settlement of a component whose members are all settled: its id is their matching number.
///
/// Created and deleted only by the reconciliation service; no default records (the reference ships
/// none).
#[model(
    name = "account.full.reconcile",
    description = "Full Reconcile",
    order = "id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
)]
pub struct AccountFullReconcile {
    /// The company of the settled items.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        readonly = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The matches of the settlement.
    #[field(
        one2many,
        comodel = "account.partial.reconcile",
        inverse_name = "full_reconcile_id",
        readonly = true,
        string = "Reconciliation Parts"
    )]
    pub partial_reconcile_ids: Vec<RecId>,
    /// The settled items.
    #[field(
        one2many,
        comodel = "account.move.line",
        inverse_name = "full_reconcile_id",
        readonly = true,
        string = "Matched Journal Items"
    )]
    pub reconciled_line_ids: Vec<RecId>,
}

/// What is still due on a matched item, in the company currency and in the item's currency, and
/// the label of its component (amendment A7). A row exists while the item is a member
/// of a component that is not settled yet; an item never matched, or settled, has none and reads
/// its residual from its own amounts or as zero. The CHECKs keep each residual between zero and the
/// item's amount, on the item's side.
///
/// Written only by the reconciliation service; no default records (the reference has no such
/// table, its residual is recomputed from the matches).
#[model(
    name = "account.open.item",
    description = "Open Journal Item",
    order = "line_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    // residual updates rewrite small rows often
    fillfactor = 70,
    constraints = [
        (
            "_open_item_line_unique",
            "UNIQUE (line_id)",
            "A journal item has one open-item row."
        ),
        (
            "_open_item_residual_company_bounds",
            "CHECK ((balance >= 0 AND amount_residual BETWEEN 0 AND balance) \
             OR (balance <= 0 AND amount_residual BETWEEN balance AND 0))",
            "A match cannot settle more than the journal item's amount."
        ),
        (
            "_open_item_residual_currency_bounds",
            "CHECK ((amount_currency >= 0 AND amount_residual_currency BETWEEN 0 AND amount_currency) \
             OR (amount_currency <= 0 AND amount_residual_currency BETWEEN amount_currency AND 0))",
            "A match cannot settle more than the journal item's amount in its currency."
        ),
    ],
)]
pub struct AccountOpenItem {
    /// The journal item.
    #[field(
        many2one,
        comodel = "account.move.line",
        required = true,
        ondelete = "restrict",
        readonly = true,
        string = "Journal Item"
    )]
    pub line_id: Option<RecId>,
    /// The item's company.
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        readonly = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// The item's account.
    #[field(
        many2one,
        comodel = "account.account",
        required = true,
        readonly = true,
        string = "Account"
    )]
    pub account_id: Option<RecId>,
    /// The item's partner.
    #[field(many2one, comodel = "res.partner", readonly = true, string = "Partner")]
    pub partner_id: Option<RecId>,
    /// The item's currency.
    #[field(
        many2one,
        comodel = "res.currency",
        required = true,
        readonly = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// The company currency.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "company_id.currency_id",
        string = "Company Currency"
    )]
    pub company_currency_id: Option<RecId>,
    /// The item's balance in the company currency.
    #[field(
        monetary,
        currency_field = "company_currency_id",
        readonly = true,
        string = "Balance"
    )]
    pub balance: Decimal,
    /// The item's amount in its currency.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Amount in Currency"
    )]
    pub amount_currency: Decimal,
    /// What is still due, in the company currency.
    #[field(
        monetary,
        currency_field = "company_currency_id",
        readonly = true,
        string = "Residual"
    )]
    pub amount_residual: Decimal,
    /// What is still due, in the item's currency.
    #[field(
        monetary,
        currency_field = "currency_id",
        readonly = true,
        string = "Residual in Currency"
    )]
    pub amount_residual_currency: Decimal,
    /// The smallest match id of the item's component (`P<id>`).
    #[field(
        integer,
        required = true,
        readonly = true,
        index = true,
        string = "Component"
    )]
    pub component_id: i32,
}
