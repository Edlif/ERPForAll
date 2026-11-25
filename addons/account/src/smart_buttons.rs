// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cross-document smart buttons owned by `account`:
//! - `account.move`: Payments (`payment_count` / `action_view_payments`)
//! - `account.payment`: Reconciled Invoices/Bills (`reconciled_invoice_count` / `action_view_reconciled_invoices`)
//!   and Journal Entry (`action_open_journal_entry`)
//! - `res.partner`: Invoices & Bills (`invoice_count` / `action_view_partner_invoices`)
//!
//! Every count shows only documents the caller may read. The partner count is one grouped read
//! whose search predicate carries the caller's read access, and its action opens a domain on
//! `partner_id`; payments and reconciled invoices are linked through reconciliation rows, which
//! no relation field names, so they filter their linked ids through
//! [`Records::filtered_access`] with [`Operation::Read`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use erp_addon_base::ResPartner;
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, Operation, ValueMap};

use crate::moves::AccountMove;
use crate::payment::AccountPayment;

/// Move id, payment id: payments directly backing this move or reconciled with any of its lines.
const MOVE_PAYMENTS: &str = "SELECT m.id::int8, p.id::int8 FROM account_move m \
    JOIN account_payment p ON p.move_id = m.id \
    WHERE m.id = ANY($1) \
    UNION \
    SELECT DISTINCT ml.move_id::int8, p.id::int8 FROM account_move_line ml \
    JOIN account_partial_reconcile pr ON pr.debit_move_id = ml.id OR pr.credit_move_id = ml.id \
    JOIN account_move_line other ON other.id = CASE WHEN pr.debit_move_id = ml.id THEN pr.credit_move_id ELSE pr.debit_move_id END \
    JOIN account_payment p ON p.move_id = other.move_id \
    WHERE ml.move_id = ANY($1)";

/// Payment id, invoice/bill move id: invoices and bills reconciled with this payment's journal entry.
const PAYMENT_INVOICES: &str = "SELECT DISTINCT p.id::int8, inv.id::int8 FROM account_payment p \
    JOIN account_move_line pl ON pl.move_id = p.move_id \
    JOIN account_partial_reconcile pr ON pr.debit_move_id = pl.id OR pr.credit_move_id = pl.id \
    JOIN account_move_line il ON il.id = CASE WHEN pr.debit_move_id = pl.id THEN pr.credit_move_id ELSE pr.debit_move_id END \
    JOIN account_move inv ON inv.id = il.move_id \
    WHERE p.id = ANY($1) AND inv.move_type IN ('out_invoice', 'out_refund', 'in_invoice', 'in_refund')";

/// The move types the partner's Invoices & Bills button counts.
const INVOICE_TYPES: [&str; 4] = ["out_invoice", "out_refund", "in_invoice", "in_refund"];

fn decode(error: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Reading linked accounting documents failed.").with_source(error)
}

fn row_of(id: &RecId) -> Option<i64> {
    id.as_real().or(id.origin()).map(|id| id.get())
}

async fn readable_links(
    tx: &mut Transaction,
    sources: &Records,
    sql: &str,
    target: &str,
) -> Result<BTreeMap<i64, Vec<i64>>> {
    let ids: Vec<i64> = sources.rec_ids().iter().filter_map(row_of).collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut links = Vec::new();
    for row in tx.query(sql, &[&ids]).await? {
        let source: i64 = row.try_get(0).map_err(decode)?;
        let linked: i64 = row.try_get(1).map_err(decode)?;
        links.push((source, linked));
    }
    let readable: BTreeSet<i64> = sources
        .env()
        .records(target)?
        .browse(links.iter().map(|(_, linked)| *linked))?
        .filtered_access(tx, Operation::Read)
        .await?
        .ids()
        .into_iter()
        .collect();
    let mut out: BTreeMap<i64, Vec<i64>> = BTreeMap::new();
    for (source, linked) in links {
        if readable.contains(&linked) {
            out.entry(source).or_default().push(linked);
        }
    }
    for linked in out.values_mut() {
        linked.sort_unstable();
        linked.dedup();
    }
    Ok(out)
}

fn counts(sources: &Records, found: &BTreeMap<i64, Vec<i64>>) -> Result<Vec<i32>> {
    sources
        .rec_ids()
        .iter()
        .map(|id| {
            let n = row_of(id).and_then(|id| found.get(&id)).map_or(0, Vec::len);
            i32::try_from(n).map_err(|_| Error::programming("document count does not fit an i32"))
        })
        .collect()
}

fn flat(found: BTreeMap<i64, Vec<i64>>) -> Vec<i64> {
    let mut all: Vec<i64> = found.into_values().flatten().collect();
    all.sort_unstable();
    all.dedup();
    all
}

fn window(name: &str, model: &str, views: Vec<(i64, &str)>, ids: Vec<i64>) -> Value {
    let context = ValueMap::from([("create".to_owned(), Value::Bool(false))]);
    if let [id] = ids.as_slice() {
        let form = views
            .iter()
            .find(|(_, kind)| *kind == "form")
            .map_or(Value::False, |(view, _)| Value::Int(*view));
        return Value::Map(Arc::new(ValueMap::from([
            ("type".to_owned(), Value::str("ir.actions.act_window")),
            ("name".to_owned(), Value::str(name)),
            ("res_model".to_owned(), Value::str(model)),
            ("target".to_owned(), Value::str("current")),
            ("res_id".to_owned(), Value::Int(*id)),
            (
                "views".to_owned(),
                Value::list([Value::list([form, Value::str("form")])]),
            ),
            ("context".to_owned(), Value::Map(Arc::new(context))),
        ])));
    }
    let views = if views.is_empty() {
        vec![
            Value::list([Value::False, Value::str("list")]),
            Value::list([Value::False, Value::str("form")]),
        ]
    } else {
        views
            .into_iter()
            .map(|(id, kind)| Value::list([Value::Int(id), Value::str(kind)]))
            .collect()
    };
    Value::Map(Arc::new(ValueMap::from([
        ("type".to_owned(), Value::str("ir.actions.act_window")),
        ("name".to_owned(), Value::str(name)),
        ("res_model".to_owned(), Value::str(model)),
        ("target".to_owned(), Value::str("current")),
        ("views".to_owned(), Value::list(views)),
        (
            "domain".to_owned(),
            Value::list([Value::list([
                Value::str("id"),
                Value::str("in"),
                Value::list(ids.into_iter().map(Value::Int)),
            ])]),
        ),
        ("context".to_owned(), Value::Map(Arc::new(context))),
    ])))
}

async fn named_views(
    tx: &mut Transaction,
    list_name: &str,
    form_name: &str,
) -> Result<Vec<(i64, &'static str)>> {
    let rows = tx
        .query(
            "SELECT name, res_id::int8 FROM ir_model_data WHERE module = 'account' \
             AND model = 'ir.ui.view' AND name IN ($1, $2)",
            &[&list_name, &form_name],
        )
        .await?;
    let mut found = Vec::with_capacity(rows.len());
    for row in &rows {
        found.push((
            row.try_get::<_, String>(0).map_err(decode)?,
            row.try_get::<_, i64>(1).map_err(decode)?,
        ));
    }
    let find = |wanted: &str| -> Result<i64> {
        found
            .iter()
            .find(|(name, _)| name == wanted)
            .map(|(_, id)| *id)
            .ok_or_else(|| Error::invariant(format!("The accounting view {wanted} is missing.")))
    };
    Ok(vec![(find(list_name)?, "list"), (find(form_name)?, "form")])
}

/// Per source record, in record order, how many `model` records the caller may read have the
/// source in `link` (a many2one) and match `filter`: one grouped read whose search predicate
/// carries the caller's read access. A caller refused read on `model` counts none, as
/// [`Records::filtered_access`] treats a model-level refusal.
///
/// # Errors
///
/// Read failures that are not refusals; a count that does not fit an `i32`.
pub async fn grouped_counts(
    tx: &mut Transaction,
    sources: &Records,
    model: &str,
    link: &str,
    filter: Domain,
) -> Result<Vec<i32>> {
    let ids: Vec<i64> = sources.rec_ids().iter().filter_map(row_of).collect();
    let domain = Domain::and([Domain::field(link)?.in_(ids), filter]);
    let read = sources
        .env()
        .records(model)?
        ._read_group(
            tx,
            ReadGroup::new(domain)
                .groupby([link])
                .aggregates(["__count"]),
        )
        .await;
    let groups = match read {
        Ok(groups) => groups,
        Err(error) if error.kind() == ErrorKind::Access => return Ok(vec![0; sources.len()]),
        Err(error) => return Err(error),
    };
    let mut found: BTreeMap<i64, i64> = BTreeMap::new();
    for (key, count) in groups.column(0).iter().zip(groups.column(1)) {
        let GroupValue::Int(count) = count else {
            return Err(Error::internal("A document count is not an integer."));
        };
        if let Some(id) = key.record_id() {
            found.insert(id.get(), *count);
        }
    }
    sources
        .rec_ids()
        .iter()
        .map(|id| {
            // a source with no group has no readable document
            let n = row_of(id)
                .and_then(|id| found.get(&id))
                .copied()
                .unwrap_or(0);
            i32::try_from(n).map_err(|_| Error::programming("document count does not fit an i32"))
        })
        .collect()
}

/// One condition of a client domain list.
pub fn leaf(field: &str, op: &str, value: Value) -> Value {
    Value::list([Value::str(field), Value::str(op), value])
}

/// A current-window list and form of `model` over `domain` (a client domain list); `views`
/// empty means the model's defaults.
pub fn domain_window(
    name: &str,
    model: &str,
    views: Vec<(i64, &str)>,
    domain: Vec<Value>,
) -> Value {
    let context = ValueMap::from([("create".to_owned(), Value::Bool(false))]);
    let views = if views.is_empty() {
        vec![
            Value::list([Value::False, Value::str("list")]),
            Value::list([Value::False, Value::str("form")]),
        ]
    } else {
        views
            .into_iter()
            .map(|(id, kind)| Value::list([Value::Int(id), Value::str(kind)]))
            .collect()
    };
    Value::Map(Arc::new(ValueMap::from([
        ("type".to_owned(), Value::str("ir.actions.act_window")),
        ("name".to_owned(), Value::str(name)),
        ("res_model".to_owned(), Value::str(model)),
        ("target".to_owned(), Value::str("current")),
        ("views".to_owned(), Value::list(views)),
        ("domain".to_owned(), Value::list(domain)),
        ("context".to_owned(), Value::Map(Arc::new(context))),
    ])))
}

/// The id of the one stored record a button acts on, after the caller's read check.
///
/// # Errors
///
/// `Access` without read on it; not exactly one record; an unsaved record.
pub async fn the_one(records: &Records, tx: &mut Transaction) -> Result<i64> {
    records.check_access(tx, Operation::Read).await?;
    records.ensure_one()?;
    records
        .rec_ids()
        .first()
        .and_then(row_of)
        .ok_or_else(|| Error::validation("Save the record before opening its linked documents."))
}

/// `account.move` with its linked payment count.
#[methods(AccountMove, addon = "account")]
impl AccountMove {
    #[compute(depends = ["state", "payment_state", "company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_payment_count(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountMove>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, MOVE_PAYMENTS, "account.payment").await?;
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, AccountMove::PAYMENT_COUNT, count)?;
        }
        Ok(())
    }

    /// Open the readable payments linked to this invoice, bill, or journal entry.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_payments(
        this: &RecordSet<AccountMove>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, MOVE_PAYMENTS, "account.payment").await?;
        Ok(window(
            "Payments",
            "account.payment",
            Vec::new(),
            flat(found),
        ))
    }
}

/// `account.payment` with its reconciled invoices/bills smart button and journal entry link.
#[methods(AccountPayment, addon = "account")]
impl AccountPayment {
    #[compute(depends = ["state", "move_id", "is_reconciled", "company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_reconciled_invoice_count(
        this: &RecordSet<AccountPayment>,
        tx: &mut Transaction,
        out: &mut Assign<'_, AccountPayment>,
    ) -> Result<()> {
        let records = this.as_records();
        let found = readable_links(tx, records, PAYMENT_INVOICES, "account.move").await?;
        let moves = this.mapped(tx, AccountPayment::MOVE_ID).await?;
        let readable_moves: BTreeSet<i64> = records
            .env()
            .records("account.move")?
            .browse(moves.iter().filter_map(|m| m.as_ref().and_then(row_of)))?
            .filtered_access(tx, Operation::Read)
            .await?
            .ids()
            .into_iter()
            .collect();
        for (index, count) in counts(records, &found)?.into_iter().enumerate() {
            out.set(index, AccountPayment::RECONCILED_INVOICE_COUNT, count)?;
            let has_entry = moves[index]
                .as_ref()
                .and_then(row_of)
                .is_some_and(|id| readable_moves.contains(&id));
            out.set(
                index,
                AccountPayment::JOURNAL_ENTRY_COUNT,
                i32::from(has_entry),
            )?;
        }
        Ok(())
    }

    /// Open the readable invoices or vendor bills settled by this payment.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_reconciled_invoices(
        this: &RecordSet<AccountPayment>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let found = readable_links(tx, records, PAYMENT_INVOICES, "account.move").await?;
        let views = named_views(tx, "view_invoice_list", "view_invoice_form").await?;
        Ok(window(
            "Invoices & Bills",
            "account.move",
            views,
            flat(found),
        ))
    }

    /// Open the journal entry created for this payment.
    #[method]
    #[rpc(effect = read)]
    async fn action_open_journal_entry(
        this: &RecordSet<AccountPayment>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let records = this.as_records();
        records.check_access(tx, Operation::Read).await?;
        records.ensure_one()?;
        let move_ids: Vec<i64> = match records.cell(tx, "move_id").await? {
            Cell::Id(id) => records
                .env()
                .records("account.move")?
                .browse([id])?
                .filtered_access(tx, Operation::Read)
                .await?
                .ids(),
            _ => Vec::new(),
        };
        let views = named_views(tx, "view_move_list", "view_move_form").await?;
        Ok(window("Journal Entry", "account.move", views, move_ids))
    }
}

/// `res.partner` with its readable invoice and vendor bill count.
#[extend(ResPartner, addon = "account")]
pub struct PartnerInvoices {
    /// Readable customer invoices, credit notes, vendor bills, and refunds of this contact.
    #[field(integer, compute = "_compute_invoice_count", string = "Invoice Count")]
    pub invoice_count: i32,
}

#[methods(ResPartner, addon = "account")]
impl PartnerInvoices {
    #[compute(depends = ["company_id"], depends_context = ["uid", "allowed_company_ids"])]
    async fn _compute_invoice_count(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ResPartner>,
    ) -> Result<()> {
        let records = this.as_records();
        let types = Domain::field("move_type")?.in_(INVOICE_TYPES.to_vec());
        let found = grouped_counts(tx, records, "account.move", "partner_id", types).await?;
        for (index, count) in found.into_iter().enumerate() {
            out.set(index, PartnerInvoices::INVOICE_COUNT, count)?;
        }
        Ok(())
    }

    /// Open this contact's invoices and vendor bills.
    #[method]
    #[rpc(effect = read)]
    async fn action_view_partner_invoices(
        this: &RecordSet<ResPartner>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let id = the_one(this.as_records(), tx).await?;
        let views = named_views(tx, "view_invoice_list", "view_invoice_form").await?;
        Ok(domain_window(
            "Invoices & Bills",
            "account.move",
            views,
            vec![
                Value::str("&"),
                leaf("partner_id", "=", Value::Int(id)),
                leaf(
                    "move_type",
                    "in",
                    Value::list(INVOICE_TYPES.into_iter().map(Value::str)),
                ),
            ],
        ))
    }
}
