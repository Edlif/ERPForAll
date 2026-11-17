// SPDX-License-Identifier: MIT OR Apache-2.0
//! The invoicing policy `sale` adds to products and companies (B3, B4): a product is
//! invoiced on ordered or on delivered quantities; a company gives the default for new products
//! and for lines without product (
//! ).

use std::collections::BTreeMap;

use erp_addon_product::ProductTemplate;
use erp_sdk::ValueMap;
use erp_sdk::prelude::*;

/// The two policies.
pub const POLICIES: [(&str, &str); 2] = [
    ("order", "Ordered quantities"),
    ("delivery", "Delivered quantities"),
];

/// `product.template` with its invoicing policy.
#[extend(ProductTemplate, addon = "sale")]
pub struct ProductTemplateSale {
    /// Ordered or delivered quantities; new products take their company's default, goods are
    /// invoiced on ordered quantities when their type is set.
    #[field(
        selection,
        selection = [("order", "Ordered quantities"), ("delivery", "Delivered quantities")],
        compute = "_compute_invoice_policy",
        store = true,
        readonly = false,
        required = true,
        string = "Invoicing Policy"
    )]
    pub invoice_policy: Option<Arc<str>>,
}

/// `res.company` with the default invoicing policy.
#[extend(ResCompany, addon = "sale")]
pub struct CompanySale {
    /// The policy of new products and of lines without product.
    #[field(
        selection,
        selection = [("order", "Ordered quantities"), ("delivery", "Delivered quantities")],
        default = "order",
        required = true,
        string = "Invoicing Policy"
    )]
    pub sale_invoice_policy: Option<Arc<str>>,
}

/// The default invoicing policy of each company of `companies`.
///
/// # Errors
/// The read's errors.
pub(crate) async fn company_policies(
    tx: &mut Transaction,
    env: &Env,
    companies: impl IntoIterator<Item = RecId>,
) -> Result<BTreeMap<RecId, String>> {
    let set = RecordSet::<ResCompany>::try_from(
        env.sudo(true).records("res.company")?.browse_ids(companies),
    )?;
    let policies = set.mapped(tx, CompanySale::SALE_INVOICE_POLICY).await?;
    Ok(set
        .as_records()
        .rec_ids()
        .iter()
        .zip(policies)
        .filter_map(|(company, policy)| Some((*company, policy?.to_string())))
        .collect())
}

/// The policy of a new product: the current company's, else ordered quantities.
async fn new_product_policy(tx: &mut Transaction, env: &Env) -> Result<String> {
    let company = match env.company_id()? {
        Some(company) => company_policies(tx, env, RecId::from_real(company))
            .await?
            .into_values()
            .next(),
        None => None,
    };
    Ok(company.unwrap_or_else(|| POLICIES[0].0.to_owned()))
}

#[methods(ProductTemplate, addon = "sale")]
impl ProductTemplateSale {
    /// The policy of a new product as its default: the field is required and its compute runs
    /// after the INSERT, so a product created without a policy (also as the template of a new
    /// variant) gets it before; a `default_invoice_policy` context key still wins.
    #[method(override)]
    async fn default_get(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<ValueMap> {
        let mut values: ValueMap = sup.call(tx, this.as_records(), (names.clone(),)).await?;
        let field = ProductTemplateSale::INVOICE_POLICY.name();
        if names.iter().any(|name| name == field) && !values.contains_key(field) {
            let policy = new_product_policy(tx, this.env()).await?;
            values.insert(field.to_owned(), Value::Str(policy.into()));
        }
        Ok(values)
    }

    /// A product without policy takes the current company's; goods take ordered quantities.
    /// Other products keep theirs.
    #[compute(depends = ["type"])]
    async fn _compute_invoice_policy(
        this: &RecordSet<ProductTemplate>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductTemplate>,
    ) -> Result<()> {
        let ids: Vec<i64> = this
            .as_records()
            .rec_ids()
            .iter()
            .filter_map(|id| id.as_real().map(|id| id.get()))
            .collect();
        let stored: BTreeMap<i64, String> = if ids.is_empty() {
            BTreeMap::new()
        } else {
            tx.query(
                "SELECT id::int8, invoice_policy FROM product_template \
                 WHERE id = ANY($1) AND invoice_policy IS NOT NULL",
                &[&ids],
            )
            .await?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect()
        };
        let kinds = this.mapped(tx, ProductTemplate::TYPE).await?;
        let env = this.env().clone();
        let default = new_product_policy(tx, &env).await?;
        for (index, (id, kind)) in this.as_records().rec_ids().iter().zip(kinds).enumerate() {
            let current = id.as_real().and_then(|id| stored.get(&id.get()));
            let policy = match current {
                None => default.clone(),
                Some(_) if kind.as_deref() == Some("consu") => POLICIES[0].0.to_owned(),
                Some(_) => continue,
            };
            out.set(
                index,
                ProductTemplateSale::INVOICE_POLICY,
                Some(Arc::<str>::from(policy)),
            )?;
        }
        Ok(())
    }
}
