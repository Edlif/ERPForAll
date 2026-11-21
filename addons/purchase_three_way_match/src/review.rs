// SPDX-License-Identifier: MIT OR Apache-2.0
//! Compare bounded, caller-visible item memberships. Read received quantity through Purchase.

use std::collections::{BTreeMap, BTreeSet};

use erp_addon_purchase::line::PurchaseOrderLine;
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, FieldOp, Operation};

const SOURCE_LIMIT: &str = "purchase.max_direct_bill_source_rows";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Advice {
    Matched,
    NoSource,
    UnitCurrency,
    Price,
    Received,
    Ordered,
    Negative,
    Cancelled,
}

impl Advice {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Matched => "Matched",
            Self::NoSource => "No linked purchase item",
            Self::UnitCurrency => "Review source, unit or currency",
            Self::Price => "Review price or discount",
            Self::Received => "Billed above received quantity",
            Self::Ordered => "Billed above ordered quantity",
            Self::Negative => "Review credit or negative quantity",
            Self::Cancelled => "Cancelled bill",
        }
    }
}

#[derive(Default)]
pub(super) struct Bill {
    pub items: BTreeMap<i64, Advice>,
    cancelled: bool,
}

impl Bill {
    pub(super) fn summary(&self) -> &'static str {
        if self.cancelled {
            "Cancelled bill"
        } else if self.items.is_empty() {
            "No bill items"
        } else if self.items.values().all(|s| *s == Advice::Matched) {
            "Matched"
        } else {
            "Review purchase items"
        }
    }
}

async fn authorize(tx: &mut Transaction, records: &Records, fields: &[&str]) -> Result<()> {
    records.check_access(tx, Operation::Read).await?;
    for field in fields {
        if !tx
            .has_field_access(records.env(), records.spec().name, field, FieldOp::Read)
            .await?
        {
            return Err(Error::access(
                "You need access to the accounting and purchase terms to review this bill.",
            ));
        }
    }
    Ok(())
}

fn sql_error(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("Could not read the purchase and bill match.").with_source(e)
}

pub(super) enum Evaluation {
    Complete(BTreeMap<i64, Bill>),
    Unverified(&'static str),
}

enum ReviewError {
    Source(Error),
    TooLarge,
}

impl From<Error> for ReviewError {
    fn from(error: Error) -> Self {
        Self::Source(error)
    }
}

pub(super) async fn evaluate(tx: &mut Transaction, env: &Env, ids: &[i64]) -> Result<Evaluation> {
    match inspect(tx, env, ids).await {
        Ok(bills) => Ok(Evaluation::Complete(bills)),
        Err(ReviewError::TooLarge) => Ok(Evaluation::Unverified(
            "Purchase match exceeds the review limit",
        )),
        Err(ReviewError::Source(error)) if error.kind() == ErrorKind::Access => Ok(
            Evaluation::Unverified("Purchase match unavailable with your access"),
        ),
        Err(ReviewError::Source(error)) => Err(error),
    }
}

async fn inspect(
    tx: &mut Transaction,
    env: &Env,
    ids: &[i64],
) -> std::result::Result<BTreeMap<i64, Bill>, ReviewError> {
    let mut output = BTreeMap::new();
    if ids.is_empty() {
        return Ok(output);
    }
    let docs = env.records("account.move")?.browse(ids.iter().copied())?;
    authorize(
        tx,
        &docs,
        &[
            "move_type",
            "state",
            "company_id",
            "currency_id",
            "invoice_line_ids",
        ],
    )
    .await?;
    tx.flush().await?;
    let kinds = docs.cells(tx, "move_type").await?;
    let states = docs.cells(tx, "state").await?;
    for ((id, kind), state) in docs.ids().into_iter().zip(kinds).zip(states) {
        if matches!(kind,Cell::Str(ref text) if text.as_ref()=="in_invoice") {
            output.insert(
                id,
                Bill {
                    cancelled: matches!(state,Cell::Str(ref text) if text.as_ref()=="cancel"),
                    ..Bill::default()
                },
            );
        }
    }
    if output.is_empty() {
        return Ok(output);
    }
    let limit = erp_sdk::params::get_int(tx, &erp_addon_purchase::ADDON, SOURCE_LIMIT).await?;
    let probe = limit
        .checked_add(1)
        .ok_or_else(|| Error::invariant("Invalid purchase match bound."))?;
    let bills: Vec<i64> = output.keys().copied().collect();
    // Read relational identities first so access rules always precede any disclosure of totals.
    let rows = tx
        .query(
            "SELECT i.id,i.move_id,s.purchase_line_id,s.order_id FROM account_move_line i
        LEFT JOIN purchase_direct_bill_line s ON s.bill_line_id=i.id
        WHERE i.move_id=ANY($1) AND i.display_type='product' ORDER BY i.id LIMIT $2",
            &[&bills, &probe],
        )
        .await?;
    let mut requested = BTreeMap::new();
    let mut sources = BTreeSet::<i64>::new();
    let mut orders = BTreeSet::<i64>::new();
    for row in &rows {
        let item: i64 = row.try_get(0).map_err(sql_error)?;
        let bill: i64 = row.try_get(1).map_err(sql_error)?;
        let source: Option<i64> = row.try_get(2).map_err(sql_error)?;
        requested.insert(item, (bill, source));
        sources.extend(source);
        orders.extend(row.try_get::<_, Option<i64>>(3).map_err(sql_error)?);
    }
    let lines = env.records("account.move.line")?;
    let amount_fields = [
        "move_id",
        "display_type",
        "product_id",
        "company_id",
        "product_uom_id",
        "currency_id",
        "price_unit",
        "discount",
        "quantity",
    ];
    authorize(
        tx,
        &lines.browse(requested.keys().copied())?,
        &amount_fields,
    )
    .await?;
    let sources: Vec<_> = sources.into_iter().collect();
    let source_records = env
        .records("purchase.order.line")?
        .browse(sources.iter().copied())?;
    if !sources.is_empty() {
        authorize(
            tx,
            &env.records("purchase.order")?.browse(orders)?,
            &["currency_id", "company_id"],
        )
        .await?;
        authorize(
            tx,
            &source_records,
            &[
                "order_id",
                "product_id",
                "uom_id",
                "product_qty",
                "qty_received",
                "price_unit",
                "discount",
            ],
        )
        .await?;
    }
    check_bound(rows.len(), limit)?;
    let siblings = tx
        .query(
            "SELECT s.bill_id,s.bill_line_id FROM purchase_direct_bill_line s
        WHERE s.purchase_line_id=ANY($1) AND s.bill_line_id IS NOT NULL ORDER BY s.id LIMIT $2",
            &[&sources, &probe],
        )
        .await?;
    let mut peer_docs = BTreeSet::<i64>::new();
    let mut peer_items = BTreeSet::<i64>::new();
    for row in &siblings {
        peer_docs.insert(row.try_get(0).map_err(sql_error)?);
        peer_items.insert(row.try_get(1).map_err(sql_error)?);
    }
    if !peer_items.is_empty() {
        authorize(
            tx,
            &env.records("account.move")?.browse(peer_docs)?,
            &["state", "move_type", "currency_id", "company_id"],
        )
        .await?;
        authorize(
            tx,
            &lines.browse(peer_items.iter().copied())?,
            &amount_fields,
        )
        .await?;
    }
    check_bound(siblings.len(), limit)?;
    let source_lines = RecordSet::<PurchaseOrderLine>::try_from(source_records)?;
    let received: BTreeMap<_, _> = source_lines
        .ids()
        .into_iter()
        .zip(
            source_lines
                .mapped(tx, PurchaseOrderLine::QTY_RECEIVED)
                .await?,
        )
        .collect();
    let peers: Vec<_> = peer_items.into_iter().collect();
    let rows=tx.query("SELECT s.bill_line_id,s.purchase_line_id,m.state,l.quantity,p.product_qty,
        l.price_unit=p.price_unit AND l.discount=p.discount,
        l.move_id=s.bill_id AND m.move_type='in_invoice' AND l.product_id=p.product_id AND
        l.product_uom_id=p.uom_id AND l.currency_id=o.currency_id AND m.currency_id=o.currency_id AND
        l.company_id=s.company_id AND m.company_id=s.company_id AND o.company_id=s.company_id AND
        p.order_id=s.order_id AND l.display_type='product'
        FROM purchase_direct_bill_line s JOIN account_move_line l ON l.id=s.bill_line_id
        JOIN account_move m ON m.id=s.bill_id JOIN purchase_order_line p ON p.id=s.purchase_line_id
        JOIN purchase_order o ON o.id=s.order_id WHERE s.bill_line_id=ANY($1)", &[&peers]).await?;
    let mut totals = BTreeMap::<i64, Decimal>::new();
    let mut incompatible = BTreeSet::new();
    let mut negative = BTreeSet::new();
    let mut terms = BTreeMap::new();
    for row in &rows {
        let item: i64 = row.try_get(0).map_err(sql_error)?;
        let source: i64 = row.try_get(1).map_err(sql_error)?;
        let state: &str = row.try_get(2).map_err(sql_error)?;
        let qty: Decimal = row.try_get(3).map_err(sql_error)?;
        let ordered: Decimal = row.try_get(4).map_err(sql_error)?;
        let price: bool = row.try_get(5).map_err(sql_error)?;
        let compatible = row.try_get::<_, Option<bool>>(6).map_err(sql_error)? == Some(true);
        terms.insert(item, (ordered, price));
        match state {
            "cancel" => continue,
            "draft" | "posted" => (),
            _ => return Err(Error::invariant("Unknown state of a linked vendor bill.").into()),
        }
        if !compatible {
            incompatible.insert(source);
            continue;
        }
        if qty < Decimal::ZERO {
            negative.insert(source);
        }
        let total = totals.entry(source).or_default();
        *total = total.checked_add(qty).ok_or_else(|| {
            Error::validation("The total billed quantity is outside its supported range.")
        })?;
    }
    for (item, (doc, source)) in requested {
        let bill = output
            .get_mut(&doc)
            .ok_or_else(|| Error::invariant("Unknown bill in the purchase match."))?;
        let advice = match source {
            _ if bill.cancelled => Advice::Cancelled,
            None => Advice::NoSource,
            Some(s) if incompatible.contains(&s) => Advice::UnitCurrency,
            Some(s) if negative.contains(&s) => Advice::Negative,
            Some(s) => {
                let (ordered, price) = terms.get(&item).ok_or_else(|| {
                    Error::invariant("A linked billing item lost its purchase terms.")
                })?;
                let ordered_qty = *ordered;
                let receipt = received
                    .get(&s)
                    .ok_or_else(|| Error::invariant("Missing received purchase quantity."))?;
                let billed = totals
                    .get(&s)
                    .ok_or_else(|| Error::invariant("Missing active billed quantity."))?;
                if *billed > *receipt {
                    Advice::Received
                } else if *billed > ordered_qty {
                    Advice::Ordered
                } else if !price {
                    Advice::Price
                } else {
                    Advice::Matched
                }
            }
        };
        bill.items.insert(item, advice);
    }
    Ok(output)
}

fn check_bound(len: usize, limit: i64) -> std::result::Result<(), ReviewError> {
    if i64::try_from(len).map_err(sql_error)? > limit {
        return Err(ReviewError::TooLarge);
    }
    Ok(())
}
