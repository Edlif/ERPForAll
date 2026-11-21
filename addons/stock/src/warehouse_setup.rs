// SPDX-License-Identifier: MIT OR Apache-2.0
//! A new warehouse and its generated topology. Left out by the caller, the company, name, short
//! name and address come from the company. A warehouse created
//! without its own locations gets a view location, five sub-locations and eight operation types
//! with their numbering. Ship-only warehouses also get a delivery route and its pull rule.
//! Generated records carry their warehouse identity (topology ADR D9). Install, upgrade and
//! company creation all go through [`provision`].
use erp_sdk::ValueMap;
use erp_sdk::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

/// Explicit policy for the first warehouse of a company currently being created.
#[derive(Default)]
struct FirstWarehousePolicies(BTreeMap<i64, Arc<str>>);

/// System parameter: how many characters of the company name a derived short name keeps.
pub(crate) const CODE_LENGTH_KEY: &str = "stock.warehouse_code_length";

/// The fields a new warehouse may derive, and the steps generation reads.
const DERIVED: [&str; 6] = [
    "company_id",
    "name",
    "code",
    "partner_id",
    "reception_steps",
    "delivery_steps",
];

/// The short name a warehouse takes from its company's name: the first `length` characters,
/// without surrounding blanks.
pub(crate) fn short_code(company: &str, length: usize) -> Result<String> {
    let code: String = company.chars().take(length).collect();
    let code = code.trim();
    if code.is_empty() {
        return Err(Error::user(
            "The company name gives no short name for its warehouse; enter one.",
        ));
    }
    Ok(code.to_owned())
}

fn id_default(defaults: &ValueMap, field: &str) -> Result<Option<i64>> {
    match defaults.get(field) {
        None | Some(Value::False | Value::Bool(false)) => Ok(None),
        Some(Value::Int(id)) if *id > 0 => Ok(Some(*id)),
        Some(other) => Err(Error::validation(format!(
            "The default of stock.warehouse.{field} must be a record id, not {other:?}."
        ))),
    }
}

/// Fill what the caller left out. A context or stored default of a field is applied by the
/// base create, so derivation fills only fields that have neither a value nor a default. The
/// steps are copied in so warehouse generation reads the values the record will store.
pub(crate) async fn fill_defaults(
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<()> {
    if vals_list
        .iter()
        .all(|vals| DERIVED.iter().all(|field| vals.contains(field)))
    {
        return Ok(());
    }
    let defaults = records.default_get(tx, &DERIVED).await?;
    let company_default = id_default(&defaults, "company_id")?;
    let mut companies = Vec::new();
    for vals in vals_list.iter_mut() {
        if !vals.contains("company_id")
            && let Some(id) = company_default
        {
            vals.insert("company_id".into(), Cell::Id(id));
        }
        for field in ["reception_steps", "delivery_steps"] {
            if !vals.contains(field)
                && let Some(Value::Str(step)) = defaults.get(field)
            {
                vals.insert(field.into(), Cell::Str(step.clone()));
            }
        }
        let open = ["name", "code", "partner_id"]
            .iter()
            .any(|field| !vals.contains(field) && !defaults.contains_key(*field));
        if open && let Some(Cell::Id(company)) = vals.get("company_id") {
            companies.push(*company);
        }
    }
    if companies.is_empty() {
        return Ok(());
    }
    companies.sort_unstable();
    companies.dedup();
    let rows = records.env().records("res.company")?.browse(companies)?;
    let names = rows.cells(tx, "name").await?;
    let partners = rows.cells(tx, "partner_id").await?;
    let company: BTreeMap<i64, (Cell, Cell)> = rows
        .ids()
        .into_iter()
        .zip(names.into_iter().zip(partners))
        .collect();
    let needs_code = vals_list
        .iter()
        .any(|vals| !vals.contains("code") && !defaults.contains_key("code"));
    let length = if needs_code {
        let length = erp_sdk::params::get_int(tx, &crate::ADDON, CODE_LENGTH_KEY).await?;
        usize::try_from(length).map_err(|_| {
            Error::programming(format!("{CODE_LENGTH_KEY} is outside its declared bounds."))
        })?
    } else {
        0
    };
    for vals in vals_list.iter_mut() {
        let Some(Cell::Id(id)) = vals.get("company_id") else {
            continue; // the base create names the missing required company
        };
        let Some((name, partner)) = company.get(id).cloned() else {
            continue; // not a company the caller can see: the base relation check refuses it
        };
        let Cell::Str(name) = name else {
            return Err(Error::internal("A company has no name."));
        };
        if !vals.contains("name") && !defaults.contains_key("name") {
            vals.insert("name".into(), Cell::Str(name.clone()));
        }
        if !vals.contains("code") && !defaults.contains_key("code") {
            vals.insert("code".into(), Cell::Str(short_code(&name, length)?.into()));
        }
        if !vals.contains("partner_id")
            && !defaults.contains_key("partner_id")
            && let Cell::Id(partner) = partner
        {
            vals.insert("partner_id".into(), Cell::Id(partner));
        }
    }
    Ok(())
}

/// The addon that owns the generated roles below.
const OWNER: &str = "stock";

/// The sub-locations under a warehouse's view location: the warehouse field each fills (also its
/// role), its name and the barcode suffix after the short name.
const LOCATIONS: [(&str, &str, &str); 5] = [
    ("lot_stock_id", "Stock", "STOCK"),
    ("wh_input_stock_loc_id", "Inbound", "INPUT"),
    ("wh_qc_stock_loc_id", "Quality Check", "QUALITY"),
    ("wh_output_stock_loc_id", "Outbound", "OUTPUT"),
    ("wh_pack_stock_loc_id", "Packing Area", "PACKING"),
];

/// The operation-type fields of a warehouse, which are also the roles of its generated types.
const TYPE_FIELDS: [&str; 8] = [
    "in_type_id",
    "qc_type_id",
    "store_type_id",
    "int_type_id",
    "pick_type_id",
    "pack_type_id",
    "out_type_id",
    "xdock_type_id",
];

/// What generation needs of one new warehouse after its locations exist.
pub(crate) struct Generated {
    company: i64,
    code: Arc<str>,
    reception: Arc<str>,
    delivery: Arc<str>,
    active: bool,
    /// (role, location id), the view location first.
    locations: Vec<(&'static str, i64)>,
}

impl Generated {
    fn location(&self, role: &str) -> Result<i64> {
        self.locations
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, id)| *id)
            .ok_or_else(|| Error::internal(format!("generated warehouse has no {role}")))
    }
}

/// Barcodes join the short name without blanks, in capitals, and a suffix.
fn barcode_prefix(code: &str) -> String {
    code.split_whitespace().collect::<String>().to_uppercase()
}

/// The caller's `default_<field>` context keys belong to the warehouse, never to the records
/// generated for it.
pub(crate) fn without_defaults(env: &Env) -> Env {
    let mut context = env.context().clone();
    let keys: Vec<String> = context
        .extra()
        .keys()
        .filter(|key| key.starts_with("default_"))
        .cloned()
        .collect();
    for key in keys {
        context.remove(&key);
    }
    env.with_context_replaced(context)
}

/// Create the view location and the five sub-locations of every warehouse in `vals_list` that
/// supplies no location of its own, in two batched creates, and put their ids into its values.
/// A warehouse that supplies its view location keeps the caller's topology. A sub-location
/// without the view location is refused: generation owns the whole tree or none of it.
pub(crate) async fn create_locations(
    records: &Records,
    tx: &mut Transaction,
    vals_list: &mut [RawVals],
) -> Result<Vec<Option<Generated>>> {
    let mut plan = Vec::with_capacity(vals_list.len());
    let mut views = Vec::new();
    for vals in vals_list.iter() {
        if vals.contains("view_location_id") {
            plan.push(None);
            continue;
        }
        if let Some((field, _, _)) = LOCATIONS.iter().find(|(f, _, _)| vals.contains(f)) {
            return Err(Error::validation(format!(
                "stock.warehouse.{field} is generated with the warehouse; supply view_location_id too or leave it out."
            )));
        }
        let (Some(Cell::Id(company)), Some(Cell::Str(code))) =
            (vals.get("company_id"), vals.get("code"))
        else {
            plan.push(None); // the base create names the missing required value
            continue;
        };
        let step = |field: &str, default: &str| match vals.get(field) {
            Some(Cell::Str(step)) => step.clone(),
            _ => Arc::from(default),
        };
        views.push(
            RawVals::new()
                .set_cell("name", Cell::Str(code.clone()))
                .set_cell("usage", Cell::Str("view".into()))
                .set_cell("company_id", Cell::Id(*company)),
        );
        plan.push(Some(Generated {
            company: *company,
            code: code.clone(),
            reception: step("reception_steps", "one_step"),
            delivery: step("delivery_steps", "ship_only"),
            active: !matches!(vals.get("active"), Some(Cell::Bool(false))),
            locations: Vec::new(),
        }));
    }
    if views.is_empty() {
        return Ok(plan);
    }
    let env = without_defaults(records.env());
    let model = env.records("stock.location")?;
    let view_ids = model.create(tx, views).await?.ids();
    // a barcode another location of the company already uses is left empty
    let mut candidates = Vec::new();
    let mut companies = Vec::new();
    for generated in plan.iter().flatten() {
        let prefix = barcode_prefix(&generated.code);
        for (_, _, suffix) in LOCATIONS {
            candidates.push(format!("{prefix}{suffix}"));
        }
        companies.push(generated.company);
    }
    let found = model
        .with_context([("active_test", Value::Bool(false))])
        .search_with(
            tx,
            &Domain::and([
                Domain::condition("barcode", Operator::In, candidates)?,
                Domain::condition("company_id", Operator::In, companies)?,
            ]),
            &SearchOptions::new().fetch_fields(["barcode", "company_id"]),
        )
        .await?;
    let mut taken = BTreeSet::new();
    for (barcode, company) in found
        .cells(tx, "barcode")
        .await?
        .into_iter()
        .zip(found.cells(tx, "company_id").await?)
    {
        if let (Cell::Str(barcode), Cell::Id(company)) = (barcode, company) {
            taken.insert((barcode.to_string(), company));
        }
    }
    let mut subs = Vec::new();
    let mut views = view_ids.into_iter();
    for generated in plan.iter_mut().flatten() {
        let view = views
            .next()
            .ok_or_else(|| Error::internal("a generated view location is missing"))?;
        generated.locations.push(("view_location_id", view));
        let prefix = barcode_prefix(&generated.code);
        let (reception, delivery) = (&*generated.reception, &*generated.delivery);
        for (field, name, suffix) in LOCATIONS {
            let active = match field {
                "lot_stock_id" => true,
                "wh_input_stock_loc_id" => reception != "one_step",
                "wh_qc_stock_loc_id" => reception == "three_steps",
                "wh_output_stock_loc_id" => delivery != "ship_only",
                _ => delivery == "pick_pack_ship",
            };
            let mut vals = RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("usage", Cell::Str("internal".into()))
                .set_cell("location_id", Cell::Id(view))
                .set_cell("company_id", Cell::Id(generated.company))
                .set_cell("active", Cell::Bool(active))
                .set_cell("replenish_location", Cell::Bool(field == "lot_stock_id"));
            let barcode = format!("{prefix}{suffix}");
            if !taken.contains(&(barcode.clone(), generated.company)) {
                vals.insert("barcode".into(), Cell::Str(barcode.into()));
            }
            subs.push(vals);
        }
    }
    let mut sub_ids = model.create(tx, subs).await?.ids().into_iter();
    for (vals, generated) in vals_list.iter_mut().zip(plan.iter_mut()) {
        let Some(generated) = generated else {
            continue;
        };
        vals.insert(
            "view_location_id".into(),
            Cell::Id(generated.locations[0].1),
        );
        for (field, _, _) in LOCATIONS {
            let id = sub_ids
                .next()
                .ok_or_else(|| Error::internal("a generated sub-location is missing"))?;
            generated.locations.push((field, id));
            vals.insert(field.into(), Cell::Id(id));
        }
    }
    Ok(plan)
}

/// One generated operation type: its role, name, kind, order offset, source, destination,
/// whether it is active and its numbering code.
type TypeRole = (
    &'static str,
    &'static str,
    &'static str,
    i64,
    Option<i64>,
    Option<i64>,
    bool,
    &'static str,
);

fn type_roles(g: &Generated, multi_locations: bool) -> Result<[TypeRole; 8]> {
    let stock = g.location("lot_stock_id")?;
    let input = g.location("wh_input_stock_loc_id")?;
    let quality = g.location("wh_qc_stock_loc_id")?;
    let output = g.location("wh_output_stock_loc_id")?;
    let pack = g.location("wh_pack_stock_loc_id")?;
    let one_step = &*g.reception == "one_step";
    let ship_only = &*g.delivery == "ship_only";
    let received = if one_step { stock } else { input };
    let shipped = if ship_only { stock } else { output };
    let stored_from = if &*g.reception == "two_steps" {
        input
    } else {
        quality
    };
    let picked_to = if &*g.delivery == "pick_ship" {
        output
    } else {
        pack
    };
    // Vendors and Customers come from the operation type's own defaults.
    Ok([
        (
            "in_type_id",
            "Receiving",
            "incoming",
            1,
            None,
            Some(received),
            true,
            "IN",
        ),
        (
            "qc_type_id",
            "Quality Check",
            "internal",
            2,
            Some(input),
            Some(quality),
            g.active && &*g.reception == "three_steps",
            "QC",
        ),
        (
            "store_type_id",
            "Put Away",
            "internal",
            3,
            Some(stored_from),
            Some(stock),
            g.active && !one_step,
            "STOR",
        ),
        (
            "int_type_id",
            "Internal Moves",
            "internal",
            4,
            Some(stock),
            Some(stock),
            multi_locations,
            "INT",
        ),
        (
            "pick_type_id",
            "Picking",
            "internal",
            5,
            Some(stock),
            Some(picked_to),
            g.active && !ship_only,
            "PICK",
        ),
        (
            "pack_type_id",
            "Packing",
            "internal",
            6,
            Some(pack),
            Some(shipped),
            g.active && &*g.delivery == "pick_pack_ship",
            "PACK",
        ),
        (
            "out_type_id",
            "Deliveries",
            "outgoing",
            7,
            Some(shipped),
            None,
            true,
            "OUT",
        ),
        (
            "xdock_type_id",
            "Cross-Docking",
            "internal",
            8,
            Some(input),
            Some(output),
            g.active && !one_step && !ship_only,
            "XD",
        ),
    ])
}

/// Create the eight operation types of every generated warehouse in one create (each with its
/// own sequence, topology ADR A1: standard during an addon install or upgrade, gapless when the
/// warehouse is made while the server runs), then record the generated identities and link the
/// warehouses and the receipt and delivery returns in set-based statements. The types' insert and
/// the identity updates are set-based; each type still costs its access check and one
/// `ir.sequence` insert, and during an install or upgrade one `CREATE SEQUENCE`, whose lock entry
/// is held until the transaction ends. An upgrade that provisions many companies at once
/// therefore needs `max_locks_per_transaction` room for eight sequences per company (not
/// measured).
pub(crate) async fn create_operation_types(
    records: &Records,
    tx: &mut Transaction,
    created: &Records,
    plan: &[Option<Generated>],
) -> Result<()> {
    let warehouses: Vec<(i64, &Generated)> = created
        .ids()
        .into_iter()
        .zip(plan)
        .filter_map(|(id, g)| g.as_ref().map(|g| (id, g)))
        .collect();
    if warehouses.is_empty() {
        return Ok(());
    }
    tx.flush().await?;
    // Internal transfers start active only where storage locations are on for every user.
    let rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM res_groups_implied_rel r \
         JOIN ir_model_data u ON u.model = 'res.groups' AND u.module = 'base' \
         AND u.name = 'group_user' AND u.res_id = r.gid \
         JOIN ir_model_data m ON m.model = 'res.groups' AND m.module = 'stock' \
         AND m.name = 'group_stock_multi_locations' AND m.res_id = r.hid)",
            &[],
        )
        .await?;
    let multi_locations: bool = rows
        .first()
        .ok_or_else(|| Error::internal("the storage-location setting probe returned no row"))?
        .try_get(0)
        .map_err(|e| Error::db("Reading the storage-location setting failed.").with_source(e))?;
    let rows = tx
        .query(
            "SELECT coalesce(max(sequence), 0)::int8 FROM stock_picking_type",
            &[],
        )
        .await?;
    let mut order: i64 = rows
        .first()
        .ok_or_else(|| Error::internal("the operation type order probe returned no row"))?
        .try_get(0)
        .map_err(|e| Error::db("Reading the operation type order failed.").with_source(e))?;
    let used: Vec<i32> = tx
        .query(
            "SELECT DISTINCT color FROM stock_picking_type WHERE warehouse_id IS NOT NULL AND color <> 0",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.try_get(0))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::db("Reading operation type colors failed.").with_source(e))?;
    let mut colors = (1..12).filter(|color| !used.contains(color));
    let mut vals_list = Vec::new();
    let mut tags: (Vec<i64>, Vec<String>) = (Vec::new(), Vec::new());
    for (warehouse, g) in &warehouses {
        let first_policy = tx
            .extension::<FirstWarehousePolicies>()
            .0
            .remove(&g.company);
        let color = colors.next().unwrap_or(0);
        let prefix = barcode_prefix(&g.code);
        for (role, name, kind, offset, source, dest, active, short) in
            type_roles(g, multi_locations)?
        {
            let sequence = i32::try_from(order + offset).map_err(|_| {
                Error::validation("Operation type order numbers are exhausted; renumber them.")
            })?;
            let mut vals = RawVals::new()
                .set_cell("name", Cell::Str(name.into()))
                .set_cell("code", Cell::Str(kind.into()))
                .set_cell("company_id", Cell::Id(g.company))
                .set_cell("warehouse_id", Cell::Id(*warehouse))
                .set_cell("sequence", Cell::Int(sequence))
                .set_cell("color", Cell::Int(color))
                .set_cell("active", Cell::Bool(active))
                .set_cell("barcode", Cell::Str(format!("{prefix}{short}").into()))
                .set_cell(
                    "sequence_code",
                    Cell::Str(format!("{}/{short}/", g.code).into()),
                )
                .set_cell("use_create_lots", Cell::Bool(role == "in_type_id"))
                .set_cell("use_existing_lots", Cell::Bool(role != "in_type_id"))
                .set_cell("print_label", Cell::Bool(role == "out_type_id"));
            if let Some(policy) = &first_policy {
                vals.insert("move_type".into(), Cell::Str(policy.clone()));
            }
            if let Some(source) = source {
                vals.insert("default_location_src_id".into(), Cell::Id(source));
            }
            if let Some(dest) = dest {
                vals.insert("default_location_dest_id".into(), Cell::Id(dest));
            }
            vals_list.push(vals);
            tags.0.push(*warehouse);
            tags.1.push(role.to_owned());
        }
        order += 8;
    }
    let env = without_defaults(records.env());
    let types = env
        .records("stock.picking.type")?
        .create(tx, vals_list)
        .await?
        .ids();
    tx.flush().await?;
    let tagged = tx
        .execute(
            "UPDATE stock_picking_type t SET generated_warehouse_id = v.wh, \
             generated_addon = $4, generated_role = v.role \
             FROM unnest($1::int8[], $2::int8[], $3::text[]) AS v(id, wh, role) WHERE t.id = v.id",
            &[&types, &tags.0, &tags.1, &OWNER],
        )
        .await?;
    let (mut ids, mut owners, mut roles) = (Vec::new(), Vec::new(), Vec::new());
    for (warehouse, g) in &warehouses {
        for (role, id) in &g.locations {
            ids.push(*id);
            owners.push(*warehouse);
            roles.push((*role).to_owned());
        }
    }
    let located = tx
        .execute(
            "UPDATE stock_location l SET generated_warehouse_id = v.wh, \
             generated_addon = $4, generated_role = v.role \
             FROM unnest($1::int8[], $2::int8[], $3::text[]) AS v(id, wh, role) WHERE l.id = v.id",
            &[&ids, &owners, &roles, &OWNER],
        )
        .await?;
    if usize::try_from(tagged).ok() != Some(types.len())
        || usize::try_from(located).ok() != Some(ids.len())
    {
        return Err(Error::internal(
            "a generated warehouse record disappeared before its identity was recorded",
        ));
    }
    let links = TYPE_FIELDS
        .iter()
        .map(|field| {
            format!(
                "{field} = (SELECT t.id FROM stock_picking_type t WHERE t.generated_warehouse_id = w.id \
                 AND t.generated_addon = $2 AND t.generated_role = '{field}')"
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let ids: Vec<i64> = warehouses.iter().map(|(id, _)| *id).collect();
    tx.execute(
        &format!("UPDATE stock_warehouse w SET {links} WHERE w.id = ANY($1)"),
        &[&ids, &OWNER],
    )
    .await?;
    tx.execute(
        "UPDATE stock_picking_type t SET return_picking_type_id = r.id FROM stock_picking_type r \
         WHERE t.generated_warehouse_id = ANY($1) AND r.generated_warehouse_id = t.generated_warehouse_id \
         AND t.generated_addon = $2 AND r.generated_addon = $2 \
         AND ((t.generated_role = 'in_type_id' AND r.generated_role = 'out_type_id') \
           OR (t.generated_role = 'out_type_id' AND r.generated_role = 'in_type_id'))",
        &[&ids, &OWNER],
    )
    .await?;
    let generated = [
        "generated_warehouse_id",
        "generated_addon",
        "generated_role",
    ];
    tx.invalidate_model("stock.location", Some(&generated))
        .await?;
    tx.invalidate_model(
        "stock.picking.type",
        Some(&[
            "generated_warehouse_id",
            "generated_addon",
            "generated_role",
            "return_picking_type_id",
        ]),
    )
    .await?;
    tx.invalidate_model("stock.warehouse", Some(&TYPE_FIELDS))
        .await?;
    create_ship_only_routes(tx, &env, &ids).await
}

/// The route belongs to the warehouse whose new topology the caller just created. Existing
/// choices survive, and the normal Stock route/rule creates still check the caller's rights.
async fn create_ship_only_routes(tx: &mut Transaction, env: &Env, ids: &[i64]) -> Result<()> {
    struct Delivery {
        warehouse: i64,
        company: i64,
        source: i64,
        destination: i64,
        picking_type: i64,
        active: bool,
        code: String,
    }

    let decode =
        |error| Error::db("Reading ship-only warehouse topology failed.").with_source(error);
    let deliveries = tx
        .query(
            "SELECT w.id, w.company_id, w.lot_stock_id, t.default_location_dest_id, \
                    t.id, w.active, w.code \
             FROM stock_warehouse w LEFT JOIN stock_picking_type t ON t.id = w.out_type_id \
             WHERE w.id = ANY($1) AND w.delivery_steps = 'ship_only' \
               AND w.delivery_route_id IS NULL ORDER BY w.id",
            &[&ids],
        )
        .await?
        .iter()
        .map(|row| {
            Ok(Delivery {
                warehouse: row.try_get(0).map_err(decode)?,
                company: row.try_get(1).map_err(decode)?,
                source: row.try_get(2).map_err(decode)?,
                destination: row.try_get(3).map_err(decode)?,
                picking_type: row.try_get(4).map_err(decode)?,
                active: row.try_get(5).map_err(decode)?,
                code: row.try_get(6).map_err(decode)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if deliveries.is_empty() {
        return Ok(());
    }

    let route_values = deliveries
        .iter()
        .map(|delivery| {
            let warehouse = RecId::from_real(delivery.warehouse)
                .ok_or_else(|| Error::invariant("A new warehouse has no persisted identity."))?;
            Ok(RawVals::new()
                .set_cell(
                    "name",
                    Cell::Str(format!("{}: Direct delivery", delivery.code).into()),
                )
                .set_cell("company_id", Cell::Id(delivery.company))
                .set_cell("active", Cell::Bool(delivery.active))
                .set_cell("product_selectable", Cell::Bool(false))
                .set_cell("warehouse_selectable", Cell::Bool(true))
                .set_commands("warehouse_ids", [Command::set([warehouse])]))
        })
        .collect::<Result<Vec<_>>>()?;
    let routes = env.records("stock.route")?.create(tx, route_values).await?;
    let route_ids = routes.ids();
    let rule_values = deliveries
        .iter()
        .zip(&route_ids)
        .map(|(delivery, route)| {
            RawVals::new()
                .set_cell(
                    "name",
                    Cell::Str(format!("{}: Stock to customer", delivery.code).into()),
                )
                .set_cell("company_id", Cell::Id(delivery.company))
                .set_cell("warehouse_id", Cell::Id(delivery.warehouse))
                .set_cell("route_id", Cell::Id(*route))
                .set_cell("active", Cell::Bool(delivery.active))
                .set_cell("action", Cell::Str("pull".into()))
                .set_cell("procure_method", Cell::Str("make_to_stock".into()))
                .set_cell("picking_type_id", Cell::Id(delivery.picking_type))
                .set_cell("location_src_id", Cell::Id(delivery.source))
                .set_cell("location_dest_id", Cell::Id(delivery.destination))
        })
        .collect();
    let rules = env.records("stock.rule")?.create(tx, rule_values).await?;
    if route_ids.len() != deliveries.len() || rules.ids().len() != deliveries.len() {
        return Err(Error::invariant(
            "A ship-only warehouse route or rule was not created.",
        ));
    }
    for ((delivery, route), rule) in deliveries.iter().zip(routes.iter()).zip(rules.iter()) {
        crate::route_behavior::set_generated_identity(
            tx,
            &route,
            delivery.warehouse,
            OWNER,
            "delivery_route_id",
        )
        .await?;
        crate::route_behavior::set_generated_identity(
            tx,
            &rule,
            delivery.warehouse,
            OWNER,
            "ship_only_delivery",
        )
        .await?;
    }

    let warehouse_ids: Vec<i64> = deliveries
        .iter()
        .map(|delivery| delivery.warehouse)
        .collect();
    let linked = tx
        .execute(
            "UPDATE stock_warehouse w SET delivery_route_id = v.route \
             FROM unnest($1::int8[], $2::int8[]) AS v(warehouse, route) \
             WHERE w.id = v.warehouse AND w.delivery_route_id IS NULL",
            &[&warehouse_ids, &route_ids],
        )
        .await?;
    if linked != deliveries.len() as u64 {
        return Err(Error::invariant(
            "A ship-only warehouse route could not be linked.",
        ));
    }
    tx.invalidate_model("stock.warehouse", Some(&["delivery_route_id"]))
        .await
}

/// Supply the one-step rule for existing standard warehouses without a configured delivery route.
pub(crate) async fn backfill_ship_only_routes(tx: &mut Transaction, env: &Env) -> Result<()> {
    let warehouse_ids = tx
        .query(
            "SELECT w.id FROM stock_warehouse w
             JOIN stock_picking_type t ON t.id = w.out_type_id AND t.warehouse_id = w.id
             JOIN stock_location source ON source.id = w.lot_stock_id
             JOIN stock_location dest ON dest.id = t.default_location_dest_id
             WHERE w.delivery_route_id IS NULL AND w.active AND w.delivery_steps = 'ship_only'
               AND t.code = 'outgoing' AND t.active AND t.company_id = w.company_id
               AND t.default_location_src_id = source.id AND source.company_id = w.company_id
               AND source.usage = 'internal' AND source.active
               AND dest.usage = 'customer' AND dest.active
               AND (dest.company_id IS NULL OR dest.company_id = w.company_id)
             ORDER BY w.id",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.try_get(0))
        .collect::<std::result::Result<Vec<i64>, _>>()
        .map_err(|e| {
            Error::db("Reading warehouses without a delivery route failed.").with_source(e)
        })?;
    create_ship_only_routes(tx, env, &warehouse_ids).await
}

/// Give each of `companies` its warehouse with the generated topology, in one batched create.
/// Install, every stock upgrade and company creation call this; the caller decides the
/// environment (company creation elevates, see `res_company`). The caller's `default_<field>`
/// context keys belong to the caller's own record, never to a generated warehouse.
pub(crate) async fn provision(tx: &mut Transaction, env: &Env, companies: &[i64]) -> Result<()> {
    if companies.is_empty() {
        return Ok(());
    }
    let vals = companies
        .iter()
        .map(|company| RawVals::new().set_cell("company_id", Cell::Id(*company)))
        .collect();
    without_defaults(env)
        .records("stock.warehouse")?
        .create(tx, vals)
        .await?;
    Ok(())
}

/// Bootstrap only the owners returned by the current company create. The creator may not
/// select them. A new owner has no company settings yet; its initial types honor the user's
/// unscoped field default, then the shared field default, then the declared shipping policy.
pub(crate) async fn provision_new(
    tx: &mut Transaction,
    env: &Env,
    companies: &[i64],
) -> Result<()> {
    if companies.is_empty() {
        return Ok(());
    }
    erp_addon_base::ir_config_setting::fence_company_field_defaults(tx).await?;
    tx.flush().await?;
    let rows = tx
        .query(
            "SELECT d.json_value FROM ir_default d JOIN ir_model_fields f ON f.id = d.field_id \
             WHERE f.model = 'stock.picking.type' AND f.name = 'move_type' \
             AND d.company_id IS NULL AND d.condition IS NULL \
             AND (d.user_id = $1 OR d.user_id IS NULL) ORDER BY d.user_id NULLS LAST, d.id LIMIT 1",
            &[&env.uid()],
        )
        .await?;
    let declaration = crate::settings::picking_policy()?;
    let policy = if let Some(row) = rows.first() {
        let encoded: String = row.try_get(0).map_err(|e| Error::db(e.to_string()))?;
        let value: String = erp_sdk::serde_json::from_str(&encoded).map_err(|e| {
            Error::validation(format!(
                "Invalid saved stock.picking.type.move_type default: {e}"
            ))
        })?;
        let policy = Value::str(&value);
        declaration.validate(&policy)?;
        policy
    } else {
        declaration.default_value().clone()
    };
    let Value::Str(policy) = policy else {
        return Err(Error::internal(
            "The declared Shipping Policy must be a selection.",
        ));
    };
    tx.extension::<FirstWarehousePolicies>()
        .0
        .extend(companies.iter().map(|id| (*id, policy.clone())));
    let result = provision(tx, env, companies).await;
    // Clean up unused entries as well if a lower warehouse override fails or supplies topology.
    for id in companies {
        tx.extension::<FirstWarehousePolicies>().0.remove(id);
    }
    result
}

/// Every company that has no warehouse, archived ones included, gets one; a company that has
/// any keeps what it has, so a second run creates and changes nothing.
pub(crate) async fn provision_missing(tx: &mut Transaction, env: &Env) -> Result<()> {
    tx.flush().await?;
    let companies: Vec<i64> = tx
        .query(
            "SELECT c.id::int8 FROM res_company c WHERE NOT EXISTS \
             (SELECT 1 FROM stock_warehouse w WHERE w.company_id = c.id) ORDER BY c.id",
            &[],
        )
        .await?
        .iter()
        .map(|row| row.try_get(0))
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| Error::db("Reading companies without a warehouse failed.").with_source(e))?;
    provision(tx, env, &companies).await
}

/// Whether Inventory's own default data is loaded: before it is, there is nothing to build a
/// warehouse from, and the install step provisions every company that exists by then.
pub(crate) async fn inventory_data_loaded(tx: &mut Transaction, env: &Env) -> Result<bool> {
    let Some(data) = env.registry().model("ir.model.data").map(|m| m.id) else {
        return Ok(false);
    };
    tx.flush_model(data).await?;
    let rows = tx
        .query(
            "SELECT 1 FROM ir_model_data WHERE module = 'stock' \
             AND name = 'stock_location_suppliers' AND model = 'stock.location'",
            &[],
        )
        .await?;
    Ok(!rows.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{barcode_prefix, short_code};

    #[test]
    fn short_code_cuts_characters_and_blanks() {
        assert_eq!(short_code("Northwind Traders", 5).unwrap(), "North");
        assert_eq!(short_code("Main Company", 5).unwrap(), "Main");
        assert_eq!(short_code("Ünïcode GmbH", 3).unwrap(), "Ünï");
        assert_eq!(short_code("Ab", 5).unwrap(), "Ab");
        assert!(short_code("   x", 3).is_err());
    }

    #[test]
    fn barcode_prefix_drops_blanks_and_capitalises() {
        assert_eq!(barcode_prefix("wh01"), "WH01");
        assert_eq!(barcode_prefix("My Co"), "MYCO");
    }
}
