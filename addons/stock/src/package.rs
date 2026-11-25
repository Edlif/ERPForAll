// SPDX-License-Identifier: MIT OR Apache-2.0
//! Packages (`stock.package`): table only. The stored computes over contained
//! quants and child packages (`complete_name`, `location_id`, `company_id`) and the name sequence
//! default are's.

use erp_sdk::{Operation, prelude::*};

use crate::picking::StockPicking;

/// `stock.package`: a physical or logical container of quants and other packages.
#[model(
    name = "stock.package",
    description = "Package",
    order = "name, id",
    parent_store = true,
    parent_name = "parent_package_id"
)]
pub struct StockPackage {
    /// Package reference.
    #[field(
        char,
        required = true,
        default = "New",
        copy = false,
        index = "trigram",
        string = "Package Reference"
    )]
    pub name: Option<Arc<str>>,
    /// Package type (dimensions, weights, sequence).
    #[field(
        many2one,
        comodel = "stock.package.type",
        index = true,
        string = "Package Type"
    )]
    pub package_type_id: Option<RecId>,
    /// Container this package is in.
    #[field(
        many2one,
        comodel = "stock.package",
        index = "btree_not_null",
        string = "Container"
    )]
    pub parent_package_id: Option<RecId>,
    /// Packages contained in this one.
    #[field(
        one2many,
        comodel = "stock.package",
        inverse_name = "parent_package_id",
        string = "Contained Packages"
    )]
    pub child_package_ids: Vec<RecId>,
    /// Container the package goes into at its destination.
    #[field(
        many2one,
        comodel = "stock.package",
        index = "btree_not_null",
        string = "Destination Container"
    )]
    pub package_dest_id: Option<RecId>,
    /// Materialized ancestry path maintained by the ORM parent store.
    #[field(char, index = true)]
    pub parent_path: Option<Arc<str>>,
    /// Total weight of the package, when set by hand.
    #[field(float, digits = "Stock Weight", string = "Shipping Weight")]
    pub shipping_weight: Decimal,
    /// Date the package was packed.
    #[field(date, default_fn = default_today, string = "Pack Date")]
    pub pack_date: Option<Date>,
}

fn default_today(_ctx: &erp_sdk::DefaultContext) -> Value {
    Value::Date(Date::today())
}

fn db_err(e: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::db("A stock package record could not be read.").with_source(e)
}

async fn next_package_name(env: &Env, tx: &mut Transaction) -> Result<String> {
    let seqs = env.sudo(true).records("ir.sequence")?;
    if let Ok(Value::Str(code)) = seqs
        .call::<_, Value>(tx, "next_by_code", ("stock.package".to_owned(),))
        .await
        && !code.is_empty()
    {
        return Ok(code.to_string());
    }
    let rows = tx
        .query(
            "SELECT COALESCE(max(id), 0)::int8 + 1 FROM stock_package",
            &[],
        )
        .await?;
    let next: i64 = rows[0].try_get(0).map_err(db_err)?;
    Ok(format!("PACK{next:07}"))
}

#[methods(StockPackage, addon = "stock")]
impl StockPackage {
    #[method(override)]
    async fn create(
        this: &RecordSet<StockPackage>,
        tx: &mut Transaction,
        mut vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        this.as_records()
            .check_access(tx, Operation::Create)
            .await?;
        let env = this.env();
        let today = Date::today();
        for vals in &mut vals_list {
            let needs_name = match vals.get("name") {
                None | Some(Cell::Null) => true,
                Some(Cell::Str(s)) => s.trim().is_empty() || s.as_ref() == "New",
                _ => false,
            };
            if needs_name {
                let name = next_package_name(env, tx).await?;
                vals.insert("name".into(), Cell::Str(name.into()));
            }
            if matches!(vals.get("pack_date"), None | Some(Cell::Null)) {
                vals.insert("pack_date".into(), Cell::Date(today));
            }
        }
        sup.call(tx, this.as_records(), (vals_list,)).await
    }
}

#[methods(StockPicking, addon = "stock")]
impl StockPicking {
    /// Pack all open, unpacked move lines with positive quantity into a new `stock.package`.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_put_in_pack(
        this: &RecordSet<StockPicking>,
        tx: &mut Transaction,
    ) -> Result<()> {
        let records = this.as_records().clone();
        records.check_access(tx, Operation::Write).await?;
        let id = records.ensure_one()?.int()?;
        tx.savepoint(async move |tx| {
            tx.flush().await?;
            let state_rows = tx
                .query("SELECT state FROM stock_picking WHERE id = $1", &[&id])
                .await?;
            let state: String = state_rows
                .first()
                .ok_or_else(|| Error::missing("Transfer not found."))?
                .try_get(0)
                .map_err(db_err)?;
            if matches!(state.as_str(), "done" | "cancel") {
                return Err(Error::user(
                    "Cannot put lines in a pack on a completed or cancelled transfer.",
                ));
            }
            let line_ids: Vec<i64> = tx
                .query(
                    "SELECT id::int8 FROM stock_move_line \
                     WHERE picking_id = $1 AND membership = 'open' AND active IS TRUE \
                       AND quantity > 0 AND result_package_id IS NULL \
                     ORDER BY id",
                    &[&id],
                )
                .await?
                .into_iter()
                .map(|r| r.try_get(0))
                .collect::<std::result::Result<Vec<i64>, _>>()
                .map_err(db_err)?;
            if line_ids.is_empty() {
                return Err(Error::user(
                    "There are no unpacked operation lines with quantity to put in a pack.",
                ));
            }
            let env = records.env();
            let pkg = env
                .records("stock.package")?
                .create(tx, vec![RawVals::new()])
                .await?;
            tx.flush().await?;
            let pkg_id = pkg
                .ids()
                .into_iter()
                .next()
                .ok_or_else(|| Error::invariant("Created stock.package returned no id."))?;
            env.records("stock.move.line")?
                .browse(line_ids)?
                .write(
                    tx,
                    RawVals::new().set_cell("result_package_id", Cell::Id(pkg_id)),
                )
                .await?;
            Ok(())
        })
        .await
    }
}
