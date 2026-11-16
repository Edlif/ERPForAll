// SPDX-License-Identifier: MIT OR Apache-2.0
//! Every read is one query per field for all the invoices together; new lines are created in one
//! call and stale ones deleted in one call. A line whose stored values already match is not
//! written.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_base::money::{MAX_CURRENCIES_PER_CALL, RoundingTable};
use erp_sdk::money::Increment;
use erp_sdk::prelude::*;

use super::{TAX_CHILDREN_TASK, direction_sign, is_invoice, is_refund, is_sale, journal_type};
use crate::moves::{id_cell, unchecked};
use crate::tax::compute::{self, AmountType, BaseLine, KERNEL_TASK, TaxDef, TaxKind};

fn real(id: i64) -> Result<RecId> {
    RecId::from_real(id)
        .ok_or_else(|| Error::invariant(format!("record id {id} is not a stored record id")))
}

fn text(cell: &Cell) -> Option<Arc<str>> {
    match cell {
        Cell::Str(text) => Some(text.clone()),
        _ => None,
    }
}

fn flag(cell: &Cell) -> bool {
    matches!(cell, Cell::Bool(true))
}

fn date(cell: &Cell) -> Option<Date> {
    match cell {
        Cell::Date(date) => Some(*date),
        _ => None,
    }
}

fn int(cell: &Cell) -> i32 {
    match cell {
        Cell::Int(value) => *value,
        _ => 0,
    }
}

fn id(cell: &Cell, field: &str) -> Result<Option<i64>> {
    id_cell(Some(cell), field)
}

fn ids(cell: &Cell) -> Vec<i64> {
    match cell {
        Cell::Ids(ids) => ids
            .iter()
            .filter_map(|id| id.as_real().map(|id| id.get()))
            .collect(),
        _ => Vec::new(),
    }
}

fn amount(cell: &Cell, field: &str) -> Result<Decimal> {
    match cell {
        Cell::Null | Cell::Bool(false) => Ok(Decimal::ZERO),
        Cell::Decimal(value) => Ok(*value),
        Cell::Int(value) => Ok(Decimal::from(*value)),
        Cell::Float(value) => Decimal::try_from(*value)
            .map_err(|e| Error::validation(format!("{field} {value} is not an exact amount: {e}"))),
        other => Err(Error::invariant(format!(
            "{field} holds {other:?} instead of an amount"
        ))),
    }
}

fn total(a: Decimal, b: Decimal) -> Result<Decimal> {
    a.checked_add(b)
        .ok_or_else(|| Error::validation("An invoice total exceeds the range of amounts."))
}

/// The header of a draft invoice.
struct Invoice {
    id: i64,
    move_type: Arc<str>,
    company: i64,
    currency: i64,
    journal: i64,
    partner: Option<i64>,
    invoice_date: Option<Date>,
    due: Option<Date>,
    date: Option<Date>,
}

/// A journal item of an invoice, as the sync reads it.
struct Line {
    id: i64,
    move_id: i64,
    kind: Arc<str>,
    account: Option<i64>,
    partner: Option<i64>,
    quantity: Decimal,
    price_unit: Decimal,
    discount: Decimal,
    taxes: Vec<i64>,
    repartition: Option<i64>,
    currency: Option<i64>,
    amount_currency: Decimal,
    balance: Decimal,
    subtotal: Decimal,
    price_total: Decimal,
    maturity: Option<Date>,
}

/// One tax repartition line of kind `tax`.
struct Repartition {
    id: i64,
    tax: i64,
    refund: bool,
    factor: Decimal,
    account: Option<i64>,
    sequence: i32,
}

/// The draft invoices among `move_ids`, with their headers.
async fn load_invoices(tx: &mut Transaction, env: &Env, move_ids: &[i64]) -> Result<Vec<Invoice>> {
    if move_ids.is_empty() {
        return Ok(Vec::new());
    }
    let moves = env
        .records("account.move")?
        .browse(move_ids.iter().copied())?;
    let types = moves.cells(tx, "move_type").await?;
    let states = moves.cells(tx, "state").await?;
    let mut chosen = Vec::new();
    let mut kinds = Vec::new();
    for ((id, kind), state) in moves.ids().into_iter().zip(types).zip(states) {
        let kind = text(&kind);
        if let Some(kind) = kind.filter(|k| is_invoice(k))
            && text(&state).as_deref() == Some("draft")
        {
            chosen.push(id);
            kinds.push(kind);
        }
    }
    if chosen.is_empty() {
        return Ok(Vec::new());
    }
    let moves = env
        .records("account.move")?
        .browse(chosen.iter().copied())?;
    let companies = moves.cells(tx, "company_id").await?;
    let currencies = moves.cells(tx, "currency_id").await?;
    let journals = moves.cells(tx, "journal_id").await?;
    let partners = moves.cells(tx, "partner_id").await?;
    let invoice_dates = moves.cells(tx, "invoice_date").await?;
    let dues = moves.cells(tx, "invoice_date_due").await?;
    let dates = moves.cells(tx, "date").await?;
    let mut invoices = Vec::with_capacity(chosen.len());
    for (i, (id, move_type)) in chosen.into_iter().zip(kinds).enumerate() {
        let missing = |field: &str| Error::invariant(format!("draft invoice {id} has no {field}"));
        invoices.push(Invoice {
            id,
            move_type,
            company: id_cell(companies.get(i), "company_id")?.ok_or_else(|| missing("company"))?,
            currency: id_cell(currencies.get(i), "currency_id")?
                .ok_or_else(|| missing("currency"))?,
            journal: id_cell(journals.get(i), "journal_id")?.ok_or_else(|| missing("journal"))?,
            partner: id_cell(partners.get(i), "partner_id")?,
            invoice_date: invoice_dates.get(i).and_then(date),
            due: dues.get(i).and_then(date),
            date: dates.get(i).and_then(date),
        });
    }
    Ok(invoices)
}

/// An invoice's journal must be of its kind: sale for customer documents, purchase for vendor
/// documents.
async fn check_journals(tx: &mut Transaction, env: &Env, invoices: &[Invoice]) -> Result<()> {
    let wanted: BTreeSet<i64> = invoices.iter().map(|i| i.journal).collect();
    let journals = env
        .records("account.journal")?
        .browse(wanted.iter().copied())?;
    let types: BTreeMap<i64, Option<Arc<str>>> = journals
        .ids()
        .into_iter()
        .zip(journals.cells(tx, "type").await?.iter().map(text))
        .collect();
    for invoice in invoices {
        let needed = journal_type(&invoice.move_type);
        let actual = types.get(&invoice.journal).cloned().flatten();
        if actual.as_deref() != Some(needed) {
            return Err(Error::validation(format!(
                "Invoice {} of type {} needs a {needed} journal; journal {} is of type {}.",
                invoice.id,
                invoice.move_type,
                invoice.journal,
                actual.as_deref().unwrap_or("none")
            )));
        }
    }
    Ok(())
}

/// The rounding increment of each invoice currency.
async fn load_increments(
    tx: &mut Transaction,
    env: &Env,
    invoices: &[Invoice],
) -> Result<BTreeMap<i64, Increment>> {
    let currencies: Vec<i64> = invoices
        .iter()
        .map(|i| i.currency)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut increments = BTreeMap::new();
    for chunk in currencies.chunks(MAX_CURRENCIES_PER_CALL) {
        let ids = chunk
            .iter()
            .map(|id| real(*id))
            .collect::<Result<Vec<_>>>()?;
        let table = RoundingTable::load(env, tx, &ids).await?;
        for id in ids {
            increments.insert(id.get(), table.get(id)?.increment().clone());
        }
    }
    Ok(increments)
}

/// Every journal item of the invoices.
async fn load_lines(tx: &mut Transaction, env: &Env, invoices: &[Invoice]) -> Result<Vec<Line>> {
    let move_ids: Vec<i64> = invoices.iter().map(|i| i.id).collect();
    let found = env
        .records("account.move.line")?
        .search(tx, &Domain::condition("move_id", Operator::In, move_ids)?)
        .await?;
    let mut columns = BTreeMap::new();
    for field in [
        "move_id",
        "display_type",
        "account_id",
        "partner_id",
        "quantity",
        "price_unit",
        "discount",
        "tax_ids",
        "tax_repartition_line_id",
        "currency_id",
        "amount_currency",
        "balance",
        "price_subtotal",
        "price_total",
        "date_maturity",
    ] {
        columns.insert(field, found.cells(tx, field).await?);
    }
    let cell = |field: &str, i: usize| -> Result<&Cell> {
        columns
            .get(field)
            .and_then(|c| c.get(i))
            .ok_or_else(|| Error::invariant(format!("journal item column {field} is short")))
    };
    let mut lines = Vec::with_capacity(found.len());
    for (i, line_id) in found.ids().into_iter().enumerate() {
        lines.push(Line {
            id: line_id,
            move_id: id(cell("move_id", i)?, "move_id")?
                .ok_or_else(|| Error::invariant(format!("journal item {line_id} has no entry")))?,
            kind: text(cell("display_type", i)?).unwrap_or_else(|| "product".into()),
            account: id(cell("account_id", i)?, "account_id")?,
            partner: id(cell("partner_id", i)?, "partner_id")?,
            quantity: amount(cell("quantity", i)?, "quantity")?,
            price_unit: amount(cell("price_unit", i)?, "price_unit")?,
            discount: amount(cell("discount", i)?, "discount")?,
            taxes: ids(cell("tax_ids", i)?),
            repartition: id(
                cell("tax_repartition_line_id", i)?,
                "tax_repartition_line_id",
            )?,
            currency: id(cell("currency_id", i)?, "currency_id")?,
            amount_currency: amount(cell("amount_currency", i)?, "amount_currency")?,
            balance: amount(cell("balance", i)?, "balance")?,
            subtotal: amount(cell("price_subtotal", i)?, "price_subtotal")?,
            price_total: amount(cell("price_total", i)?, "price_total")?,
            maturity: date(cell("date_maturity", i)?),
        });
    }
    Ok(lines)
}

/// The taxes of `tax_ids` as the computation needs them, with their labels, and their `tax`
/// repartition lines. A group of taxes waits for [`TAX_CHILDREN_TASK`] and a tax with a negative
/// factor (reverse charge) for [`KERNEL_TASK`]; both are refused.
async fn load_taxes(
    tx: &mut Transaction,
    env: &Env,
    tax_ids: &BTreeSet<i64>,
) -> Result<(BTreeMap<i64, (TaxDef, Arc<str>)>, Vec<Repartition>)> {
    if tax_ids.is_empty() {
        return Ok((BTreeMap::new(), Vec::new()));
    }
    let taxes = env
        .records("account.tax")?
        .browse(tax_ids.iter().copied())?;
    let mut columns = BTreeMap::new();
    for field in [
        "amount_type",
        "amount",
        "sequence",
        "price_include",
        "include_base_amount",
        "is_base_affected",
        "has_negative_factor",
        "tax_label",
    ] {
        columns.insert(field, taxes.cells(tx, field).await?);
    }
    let cell = |field: &str, i: usize| -> Result<&Cell> {
        columns
            .get(field)
            .and_then(|c| c.get(i))
            .ok_or_else(|| Error::invariant(format!("tax column {field} is short")))
    };
    let mut defs = BTreeMap::new();
    for (i, tax_id) in taxes.ids().into_iter().enumerate() {
        let label = text(cell("tax_label", i)?).unwrap_or_else(|| "".into());
        let kind = match text(cell("amount_type", i)?).as_deref() {
            Some("group") => {
                return Err(Error::user(format!(
                    "Tax {label} is a group of taxes: groups need their child taxes, which come \
                     with task {TAX_CHILDREN_TASK}."
                )));
            }
            Some(kind) => AmountType::parse(kind)?,
            None => return Err(Error::invariant(format!("tax {tax_id} has no amount type"))),
        };
        if flag(cell("has_negative_factor", i)?) {
            return Err(Error::user(format!(
                "Tax {label} has a negative distribution (reverse charge), which comes with task \
                 {KERNEL_TASK}."
            )));
        }
        let def = TaxDef {
            id: tax_id,
            sequence: int(cell("sequence", i)?),
            kind: TaxKind::Simple(kind),
            amount: amount(cell("amount", i)?, "amount")?,
            price_include: flag(cell("price_include", i)?),
            include_base_amount: flag(cell("include_base_amount", i)?),
            is_base_affected: flag(cell("is_base_affected", i)?),
        };
        defs.insert(tax_id, (def, label));
    }
    let found = env
        .records("account.tax.repartition.line")?
        .search(
            tx,
            &Domain::and([
                Domain::condition(
                    "tax_id",
                    Operator::In,
                    tax_ids.iter().copied().collect::<Vec<_>>(),
                )?,
                Domain::condition("repartition_type", Operator::Eq, "tax")?,
            ]),
        )
        .await?;
    let owners = found.cells(tx, "tax_id").await?;
    let documents = found.cells(tx, "document_type").await?;
    let factors = found.cells(tx, "factor_percent").await?;
    let accounts = found.cells(tx, "account_id").await?;
    let sequences = found.cells(tx, "sequence").await?;
    let mut repartitions = Vec::with_capacity(found.len());
    for (i, rep_id) in found.ids().into_iter().enumerate() {
        let get = |cells: &[Cell]| cells.get(i).cloned().unwrap_or(Cell::Null);
        repartitions.push(Repartition {
            id: rep_id,
            tax: id(&get(&owners), "tax_id")?
                .ok_or_else(|| Error::invariant(format!("repartition line {rep_id} has no tax")))?,
            refund: text(&get(&documents)).as_deref() == Some("refund"),
            factor: amount(&get(&factors), "factor_percent")?,
            account: id(&get(&accounts), "account_id")?,
            sequence: int(&get(&sequences)),
        });
    }
    repartitions.sort_by_key(|r| (r.tax, r.sequence, r.id));
    Ok((defs, repartitions))
}

/// Splits a tax amount over the tax's repartition lines of the document kind by their factor; the
/// rounding difference goes to the first line so the parts add up to the rounded whole.
fn distribute<'a>(
    amount: Decimal,
    repartitions: impl Iterator<Item = &'a Repartition>,
    increment: &Increment,
) -> Result<Vec<(&'a Repartition, Decimal)>> {
    let round = |value: Decimal| -> Result<Decimal> {
        Ok(increment.quantize(value, erp_addon_base::money::CURRENCY_ROUNDING_MODE)?)
    };
    let range = || Error::validation("A tax amount exceeds the range of amounts.");
    let hundred = Decimal::ONE_HUNDRED;
    let mut parts = Vec::new();
    let mut factors = Decimal::ZERO;
    let mut shared = Decimal::ZERO;
    for rep in repartitions {
        let part = round(
            amount
                .checked_mul(rep.factor)
                .and_then(|v| v.checked_div(hundred))
                .ok_or_else(range)?,
        )?;
        factors = total(factors, rep.factor)?;
        shared = total(shared, part)?;
        parts.push((rep, part));
    }
    let whole = round(
        amount
            .checked_mul(factors)
            .and_then(|v| v.checked_div(hundred))
            .ok_or_else(range)?,
    )?;
    if let Some(first) = parts.first_mut() {
        first.1 = total(first.1, whole.checked_sub(shared).ok_or_else(range)?)?;
    }
    Ok(parts)
}

/// Rebuilds the tax lines, the receivable/payable line and the totals of the draft invoices among
/// `move_ids` from their product lines. Other entries and posted documents are left alone.
///
/// # Errors
///
/// A user error naming what to configure when a journal, account or tax is missing or not yet
/// supported; the errors of the line writes.
pub async fn sync_lines(tx: &mut Transaction, env: &Env, move_ids: &[i64]) -> Result<()> {
    let invoices = load_invoices(tx, env, move_ids).await?;
    if invoices.is_empty() {
        return Ok(());
    }
    check_journals(tx, env, &invoices).await?;
    let increments = load_increments(tx, env, &invoices).await?;
    let lines = load_lines(tx, env, &invoices).await?;
    let tax_ids: BTreeSet<i64> = lines
        .iter()
        .filter(|l| l.kind.as_ref() == "product")
        .flat_map(|l| l.taxes.iter().copied())
        .collect();
    let (taxes, repartitions) = load_taxes(tx, env, &tax_ids).await?;
    let items = unchecked(&env.records("account.move.line")?);
    let mut creates = Vec::new();
    let mut deletes = Vec::new();
    let mut totals = Vec::with_capacity(invoices.len());
    for invoice in &invoices {
        let sign = direction_sign(&invoice.move_type);
        let refund = is_refund(&invoice.move_type);
        let increment = increments
            .get(&invoice.currency)
            .ok_or_else(|| Error::invariant("an invoice currency has no loaded rounding"))?;
        let own: Vec<&Line> = lines.iter().filter(|l| l.move_id == invoice.id).collect();
        let mut untaxed = Decimal::ZERO;
        let mut by_key: BTreeMap<(i64, i64), (Decimal, Arc<str>)> = BTreeMap::new();
        for line in own.iter().filter(|l| l.kind.as_ref() == "product") {
            let defs = line
                .taxes
                .iter()
                .map(|t| {
                    taxes
                        .get(t)
                        .map(|(def, _)| def.clone())
                        .ok_or_else(|| Error::invariant(format!("tax {t} was not loaded")))
                })
                .collect::<Result<Vec<_>>>()?;
            let result = compute::compute(&BaseLine {
                price_unit: line.price_unit,
                quantity: line.quantity,
                discount: line.discount,
                taxes: &defs,
                increment,
            })?;
            untaxed = total(untaxed, result.total_excluded)?;
            let mut update = RawVals::new();
            let amount_currency = sign * result.total_excluded;
            amounts_in(&mut update, line, invoice, amount_currency);
            if line.subtotal != result.total_excluded {
                update.insert(
                    "price_subtotal".into(),
                    Cell::Decimal(result.total_excluded),
                );
            }
            if line.price_total != result.total_included {
                update.insert("price_total".into(), Cell::Decimal(result.total_included));
            }
            if line.partner != invoice.partner {
                update.insert(
                    "partner_id".into(),
                    invoice.partner.map_or(Cell::Null, Cell::Id),
                );
            }
            if !update.is_empty() {
                // shortcut: one write per changed line; a batched multi-row update needs an ORM
                // bulk-write API, add it when invoices with hundreds of lines show up in a profile
                items.browse([line.id])?.write(tx, update).await?;
            }
            let base_account = line.account.ok_or_else(|| {
                Error::invariant(format!("invoice line {} has no account", line.id))
            })?;
            for tax in &result.taxes {
                let label = taxes
                    .get(&tax.tax_id)
                    .map(|(_, label)| label.clone())
                    .unwrap_or_else(|| "".into());
                let own_reps = repartitions
                    .iter()
                    .filter(|r| r.tax == tax.tax_id && r.refund == refund);
                for (rep, part) in distribute(tax.amount, own_reps, increment)? {
                    let key = (rep.id, rep.account.unwrap_or(base_account));
                    let slot = by_key.entry(key).or_insert((Decimal::ZERO, label.clone()));
                    slot.0 = total(slot.0, part)?;
                }
            }
        }
        // tax lines: one per (repartition line, account); zero amounts make no line
        let mut tax_total = Decimal::ZERO;
        let mut kept = BTreeSet::new();
        for ((rep, account), (amount, label)) in &by_key {
            if amount.is_zero() {
                continue;
            }
            tax_total = total(tax_total, *amount)?;
            let amount_currency = sign * *amount;
            let existing = own.iter().find(|l| {
                l.kind.as_ref() == "tax"
                    && l.repartition == Some(*rep)
                    && l.account == Some(*account)
                    && !kept.contains(&l.id)
            });
            match existing {
                Some(line) => {
                    kept.insert(line.id);
                    let mut update = RawVals::new();
                    amounts_in(&mut update, line, invoice, amount_currency);
                    if line.partner != invoice.partner {
                        update.insert(
                            "partner_id".into(),
                            invoice.partner.map_or(Cell::Null, Cell::Id),
                        );
                    }
                    if !update.is_empty() {
                        items.browse([line.id])?.write(tx, update).await?;
                    }
                }
                None => creates.push(
                    RawVals::new()
                        .set_cell("move_id", Cell::Id(invoice.id))
                        .set_cell("display_type", Cell::Str("tax".into()))
                        .set_cell("name", Cell::Str(label.clone()))
                        .set_cell("account_id", Cell::Id(*account))
                        .set_cell("tax_repartition_line_id", Cell::Id(*rep))
                        .set_cell("partner_id", invoice.partner.map_or(Cell::Null, Cell::Id))
                        .set_cell("amount_currency", Cell::Decimal(amount_currency)),
                ),
            }
        }
        deletes.extend(
            own.iter()
                .filter(|l| l.kind.as_ref() == "tax" && !kept.contains(&l.id))
                .map(|l| l.id),
        );
        totals.push((invoice, untaxed, tax_total));
    }
    if !deletes.is_empty() {
        items.browse(deletes.drain(..))?.unlink(tx).await?;
    }
    if !creates.is_empty() {
        items.create(tx, std::mem::take(&mut creates)).await?;
    }
    sync_terms(tx, env, &items, &lines, &totals).await?;
    // the header totals, written past the entry's write override (they trigger no sync)
    let moves = env.records("account.move")?;
    for (invoice, untaxed, tax) in &totals {
        let amount_total = total(*untaxed, *tax)?;
        let mut vals = RawVals::new()
            .set_cell("amount_untaxed", Cell::Decimal(*untaxed))
            .set_cell("amount_tax", Cell::Decimal(*tax))
            .set_cell("amount_total", Cell::Decimal(amount_total))
            .set_cell("amount_residual", Cell::Decimal(amount_total))
            .set_cell("payment_state", Cell::Str("not_paid".into()));
        if let Some(due) = due_date(invoice)
            && invoice.due != Some(due)
        {
            vals.insert("invoice_date_due".into(), Cell::Date(due));
        }
        moves.browse([invoice.id])?.write_base(tx, vals).await?;
    }
    Ok(())
}

/// Puts the amount in currency into `update` when it or the line's currency differs from the
/// invoice's; a currency change writes both, so the line write converts the balance at the new
/// currency's rate (a line keeps its currency otherwise, `moves/line.rs` create).
fn amounts_in(update: &mut RawVals, line: &Line, invoice: &Invoice, amount_currency: Decimal) {
    let currency_changed = line.currency != Some(invoice.currency);
    if currency_changed {
        update.insert("currency_id".into(), Cell::Id(invoice.currency));
    }
    if currency_changed || line.amount_currency != amount_currency {
        update.insert("amount_currency".into(), Cell::Decimal(amount_currency));
    }
}

/// The due date until payment terms exist: the one set by hand unless it is empty or before the
/// invoice date, else the invoice date, else the accounting date (partial).
fn due_date(invoice: &Invoice) -> Option<Date> {
    let start = invoice.invoice_date.or(invoice.date);
    match (invoice.due, start) {
        (Some(due), Some(start)) if due < start => Some(start),
        (Some(due), _) => Some(due),
        (None, start) => start,
    }
}

/// One receivable (customer documents) or payable (vendor documents) line per invoice that
/// balances the entry in both currencies; none when the entry is already zero (
/// partial: one installment, no payment terms). Its account is the one an existing line carries
/// when it is of the right type, else the company's first account of that type in chart order
/// (step 4; the partner properties of
/// steps 2-3 do not exist yet), else a refusal naming what to create.
async fn sync_terms(
    tx: &mut Transaction,
    env: &Env,
    items: &Records,
    lines: &[Line],
    totals: &[(&Invoice, Decimal, Decimal)],
) -> Result<()> {
    tx.flush().await?;
    let move_ids: Vec<i64> = totals.iter().map(|(i, _, _)| i.id).collect();
    let rows = tx
        .query(
            "SELECT move_id, COALESCE(SUM(balance), 0)::text, COALESCE(SUM(amount_currency), 0)::text \
             FROM account_move_line WHERE move_id = ANY($1::int8[]) AND display_type <> 'payment_term' \
             GROUP BY move_id",
            &[&move_ids],
        )
        .await?;
    let parse = |text: String| -> Result<Decimal> {
        text.parse()
            .map_err(|e| Error::db(format!("An entry sum {text:?} could not be read: {e}")))
    };
    let mut sums: BTreeMap<i64, (Decimal, Decimal)> = BTreeMap::new();
    for row in &rows {
        let id: i64 = row
            .try_get(0)
            .map_err(|e| Error::db(format!("An entry id could not be read: {e}")))?;
        let balance: String = row
            .try_get(1)
            .map_err(|e| Error::db(format!("An entry balance could not be read: {e}")))?;
        let amount: String = row
            .try_get(2)
            .map_err(|e| Error::db(format!("An entry amount could not be read: {e}")))?;
        sums.insert(id, (parse(balance)?, parse(amount)?));
    }
    let term_accounts: Vec<i64> = lines
        .iter()
        .filter(|l| l.kind.as_ref() == "payment_term")
        .filter_map(|l| l.account)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let account_model = env.records("account.account")?;
    let known = account_model.browse(term_accounts.iter().copied())?;
    let types: BTreeMap<i64, Option<Arc<str>>> = known
        .ids()
        .into_iter()
        .zip(known.cells(tx, "account_type").await?.iter().map(text))
        .collect();
    let mut fallback: BTreeMap<(i64, &'static str), i64> = BTreeMap::new();
    let mut creates = Vec::new();
    let mut deletes = Vec::new();
    for (invoice, _, _) in totals {
        let (balance, amount) = sums.get(&invoice.id).copied().unwrap_or_default();
        let terms: Vec<&Line> = lines
            .iter()
            .filter(|l| l.move_id == invoice.id && l.kind.as_ref() == "payment_term")
            .collect();
        if balance.is_zero() && amount.is_zero() {
            deletes.extend(terms.iter().map(|l| l.id));
            continue;
        }
        let wanted: &'static str = if is_sale(&invoice.move_type) {
            "asset_receivable"
        } else {
            "liability_payable"
        };
        let current = terms
            .first()
            .and_then(|l| l.account)
            .filter(|a| types.get(a).cloned().flatten().as_deref() == Some(wanted));
        let account = match current {
            Some(account) => account,
            None => match fallback.get(&(invoice.company, wanted)) {
                Some(account) => *account,
                None => {
                    let found = account_model
                        .search(
                            tx,
                            &Domain::and([
                                Domain::condition("account_type", Operator::Eq, wanted)?,
                                Domain::condition(
                                    "company_ids",
                                    Operator::In,
                                    vec![invoice.company],
                                )?,
                            ]),
                        )
                        .await?;
                    let account = found.ids().first().copied().ok_or_else(|| {
                        let kind = if wanted == "asset_receivable" {
                            "receivable"
                        } else {
                            "payable"
                        };
                        Error::user(format!(
                            "Company {} has no {kind} account: create one in Accounting, \
                             Configuration, Chart of Accounts before saving this invoice.",
                            invoice.company
                        ))
                    })?;
                    fallback.insert((invoice.company, wanted), account);
                    account
                }
            },
        };
        let due = due_date(invoice);
        let (term_balance, term_amount) = (-balance, -amount);
        match terms.split_first() {
            Some((term, rest)) => {
                deletes.extend(rest.iter().map(|l| l.id));
                let same = term.balance == term_balance
                    && term.amount_currency == term_amount
                    && term.currency == Some(invoice.currency)
                    && term.account == Some(account)
                    && term.partner == invoice.partner
                    && term.maturity == due;
                if !same {
                    let vals = RawVals::new()
                        .set_cell("currency_id", Cell::Id(invoice.currency))
                        .set_cell("account_id", Cell::Id(account))
                        .set_cell("partner_id", invoice.partner.map_or(Cell::Null, Cell::Id))
                        .set_cell("date_maturity", due.map_or(Cell::Null, Cell::Date))
                        .set_cell("balance", Cell::Decimal(term_balance))
                        .set_cell("amount_currency", Cell::Decimal(term_amount));
                    items.browse([term.id])?.write(tx, vals).await?;
                }
            }
            None => creates.push(
                RawVals::new()
                    .set_cell("move_id", Cell::Id(invoice.id))
                    .set_cell("display_type", Cell::Str("payment_term".into()))
                    .set_cell("account_id", Cell::Id(account))
                    .set_cell("partner_id", invoice.partner.map_or(Cell::Null, Cell::Id))
                    .set_cell("date_maturity", due.map_or(Cell::Null, Cell::Date))
                    .set_cell("balance", Cell::Decimal(term_balance))
                    .set_cell("amount_currency", Cell::Decimal(term_amount)),
            ),
        }
    }
    if !deletes.is_empty() {
        items.browse(deletes)?.unlink(tx).await?;
    }
    if !creates.is_empty() {
        items.create(tx, creates).await?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "test_sync.rs"]
mod tests;
