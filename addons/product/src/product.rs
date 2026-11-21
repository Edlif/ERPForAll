// SPDX-License-Identifier: MIT OR Apache-2.0
//! Product Variant model (`product.product`).

use crate::attribute::{ProductAttribute, ProductAttributeValue};
use crate::template_attribute::{ProductTemplateAttributeLine, ProductTemplateAttributeValue};
use erp_sdk::prelude::*;
use std::collections::{BTreeSet, HashMap};

/// `product.product`: Concrete sellable and inventory-tracked product variant.
/// Delegates all catalog master fields to `product.template` via `_inherits`.
#[model(
    name = "product.product",
    description = "Product Variant",
    inherits = [("product.template", "product_tmpl_id")],
    order = "default_code, id",
    rec_name = "display_name",
    rec_names_search = ["default_code", "barcode", "name"],
    company_access = parent(edge = "product_tmpl_id"),
    unique_indexes = [(
        "_combination_unique",
        "(product_tmpl_id, combination_indices) WHERE active IS TRUE",
        "A product variant with the same attribute values already exists for this product."
    )],
)]
pub struct ProductProduct {
    /// Delegation parent template.
    #[field(
        many2one,
        comodel = "product.template",
        required = true,
        ondelete = "cascade",
        index = true,
        string = "Product Template"
    )]
    pub product_tmpl_id: Option<RecId>,

    /// Internal reference SKU.
    #[field(char, index = true, string = "Internal Reference")]
    pub default_code: Option<Arc<str>>,

    /// Product barcode, unique per company.
    #[field(char, index = "btree_not_null", copy = false)]
    pub barcode: Option<Arc<str>>,

    /// Serialized sorted comma-separated PTAV IDs ('12,45,89').
    #[field(
        char,
        compute = "_compute_combination_indices",
        store = true,
        index = true
    )]
    pub combination_indices: Option<Arc<str>>,

    /// Sum of attribute value extra prices for this variant.
    #[field(
        float,
        digits = "Product Price",
        compute = "_compute_price_extra",
        string = "Variant Price Extra"
    )]
    pub price_extra: Decimal,

    /// Catalog sales price: template list_price + price_extra, writable for manual override.
    #[field(
        float,
        digits = "Product Price",
        compute = "_compute_lst_price",
        inverse = "_set_lst_price",
        store = true,
        readonly = false,
        string = "Sales Price"
    )]
    pub lst_price: Decimal,

    /// Inventory valuation standard cost. Read it through
    /// [`ProductProductMethods::_get_standard_price`] so a valuation addon can supply it. Not
    /// company-dependent yet: the ORM stores company-dependent columns as jsonb and cannot write
    /// a numeric value into one (stays open, see the review record).
    #[field(float, digits = "Product Price", string = "Cost")]
    pub standard_price: Decimal,

    /// Packaging rows (unit and barcode) of this variant.
    #[field(
        one2many,
        comodel = "product.uom",
        inverse_name = "product_id",
        string = "Unit Barcode"
    )]
    pub product_uom_ids: Vec<RecId>,

    /// Variant-specific packaging units, in addition to the template's units.
    #[field(
        many2many,
        comodel = "uom.uom",
        relation = "product_variant_extra_uom_rel",
        column1 = "product_id",
        column2 = "uom_id",
        string = "Extra Packagings"
    )]
    pub extra_uom_ids: Vec<RecId>,

    /// Active flag.
    #[field(boolean, default = true)]
    pub active: bool,

    /// Context-aware formatted display name.
    #[field(char, compute = "_compute_display_name")]
    pub display_name: Option<Arc<str>>,

    /// Internal or reader-authorized partner-specific reference.
    #[field(char, compute = "_compute_partner_reference")]
    pub code: Option<Arc<str>>,

    /// Partner-specific description with its reference, or the ordinary display name.
    #[field(char, compute = "_compute_partner_reference")]
    pub partner_ref: Option<Arc<str>>,

    /// Template attribute values forming this variant combination.
    #[field(
        many2many,
        comodel = "product.template.attribute.value",
        relation = "product_variant_combination",
        string = "Attribute Values"
    )]
    pub product_template_attribute_value_ids: Vec<RecId>,

    /// The variant's own picture, at most 1920 x 1920 pixels. Empty for a variant that shows the
    /// picture of its template.
    #[field(image, max_width = 1920, max_height = 1920, string = "Variant Image")]
    pub image_variant_1920: Option<Blob>,

    /// The variant's own picture at most 1024 x 1024 pixels, stored.
    #[field(
        image,
        max_width = 1024,
        max_height = 1024,
        related = "image_variant_1920",
        store = true,
        string = "Variant Image 1024"
    )]
    pub image_variant_1024: Option<Blob>,

    /// The variant's own picture at most 512 x 512 pixels, stored.
    #[field(
        image,
        max_width = 512,
        max_height = 512,
        related = "image_variant_1920",
        store = true,
        string = "Variant Image 512"
    )]
    pub image_variant_512: Option<Blob>,

    /// The variant's own picture at most 256 x 256 pixels, stored.
    #[field(
        image,
        max_width = 256,
        max_height = 256,
        related = "image_variant_1920",
        store = true,
        string = "Variant Image 256"
    )]
    pub image_variant_256: Option<Blob>,

    /// The variant's own picture at most 128 x 128 pixels, stored.
    #[field(
        image,
        max_width = 128,
        max_height = 128,
        related = "image_variant_1920",
        store = true,
        string = "Variant Image 128"
    )]
    pub image_variant_128: Option<Blob>,

    /// The picture shown for the variant: its own picture, otherwise the picture of its template.
    /// This own field replaces the delegated template field of the same
    /// name. It is not stored, so it carries no bounds: its sources are already scaled. A write
    /// goes to the template or to the variant's own picture, see `_set_image_1920`.
    #[field(
        image,
        compute = "_compute_image_1920",
        inverse = "_set_image_1920",
        readonly = false,
        string = "Image"
    )]
    pub image_1920: Option<Blob>,

    /// The shown picture at most 1024 x 1024 pixels: the variant's own stored size, otherwise
    /// the template's.
    #[field(image, compute = "_compute_image_1024", string = "Image 1024")]
    pub image_1024: Option<Blob>,

    /// The shown picture at most 512 x 512 pixels: the variant's own stored size, otherwise
    /// the template's.
    #[field(image, compute = "_compute_image_512", string = "Image 512")]
    pub image_512: Option<Blob>,

    /// The shown picture at most 256 x 256 pixels: the variant's own stored size, otherwise
    /// the template's.
    #[field(image, compute = "_compute_image_256", string = "Image 256")]
    pub image_256: Option<Blob>,

    /// The shown picture at most 128 x 128 pixels: the variant's own stored size, otherwise
    /// the template's.
    #[field(image, compute = "_compute_image_128", string = "Image 128")]
    pub image_128: Option<Blob>,
}

#[methods(ProductProduct, addon = "product")]
impl ProductProduct {
    /// Unit cost of every variant of the set, in the company of the environment (Hook N6, the
    /// cost valuation boundary).
    ///
    /// The default reads the stored `standard_price` (not yet company-dependent,
    /// open). An addon that values
    /// stock per row (for example `stock_account`) overrides this method with `#[method(override)]`
    /// and answers from its own state without writing the product row on every receipt; the
    /// override calls `sup` for the rows it does not value. Every cost read of this addon goes
    /// through this method: the pricelist cost base and the template `standard_price`.
    ///
    /// The result has one entry per record, in the order of the set.
    #[method]
    async fn _get_standard_price(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
    ) -> Result<Vec<Decimal>> {
        this.mapped(tx, ProductProduct::STANDARD_PRICE).await
    }

    /// Labels use reader-authorized supplier rows, loaded once for this batch.
    #[compute(depends = [
        "name", "default_code", "product_tmpl_id", "product_template_attribute_value_ids",
        "product_template_attribute_value_ids.product_attribute_value_id.name",
        "product_tmpl_id.seller_ids.product_name", "product_tmpl_id.seller_ids.product_code",
        "product_tmpl_id.seller_ids.partner_id", "product_tmpl_id.seller_ids.company_id",
        "product_tmpl_id.seller_ids.product_id", "product_tmpl_id.seller_ids.sequence",
        "product_tmpl_id.seller_ids.min_qty", "product_tmpl_id.seller_ids.price"
    ], depends_context = ["display_default_code", "formatted_display_name", "partner_id", "seller_id", "company_id", "lang", "label_reader"])]
    async fn _compute_display_name(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let names = crate::names::template_names(this, tx).await?;
        let codes = this.mapped(tx, ProductProduct::DEFAULT_CODE).await?;
        let templates = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
        let template_ids: Vec<i64> = templates
            .iter()
            .flatten()
            .filter(|id| !id.is_new())
            .map(|id| id.get())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let sellers = crate::names::sellers(this.env(), tx, &template_ids).await?;
        let combinations = combination_names(this, tx).await?;
        let company = crate::names::context_id(this.env(), "company_id")?;
        let explicit = crate::names::context_id(this.env(), "seller_id")?;
        for index in 0..this.len() {
            let variant = this.as_records().rec_ids()[index].get();
            let template = templates[index]
                .ok_or_else(|| Error::validation("Product variant missing template link"))?
                .get();
            let name = names[index].as_deref().unwrap_or("");
            let eligible: Vec<_> = sellers
                .iter()
                .filter(|row| {
                    row.template == template && row.variant.is_none_or(|id| id == variant)
                })
                .collect();
            let selected: Vec<_> = if let Some(id) = explicit {
                eligible
                    .iter()
                    .copied()
                    .filter(|row| row.id == id)
                    .filter(|row| company.is_none_or(|id| row.company.is_none_or(|c| c == id)))
                    .collect()
            } else {
                let specific = eligible.iter().any(|row| row.variant == Some(variant));
                eligible
                    .iter()
                    .copied()
                    .filter(|row| {
                        if specific {
                            row.variant == Some(variant)
                        } else {
                            row.variant.is_none()
                        }
                    })
                    .filter(|row| company.is_none_or(|id| row.company.is_none_or(|c| c == id)))
                    .collect()
            };
            let append_combination = |base: &str| {
                if combinations[index].is_empty() {
                    base.to_owned()
                } else {
                    format!("{base} ({})", combinations[index])
                }
            };
            let mut labels = Vec::new();
            for row in selected {
                let label = crate::names::format_label(
                    this.env(),
                    &append_combination(row.name.as_deref().unwrap_or(name)),
                    row.code.as_deref().or(codes[index].as_deref()),
                )?;
                if !labels.contains(&label) {
                    labels.push(label);
                }
            }
            if labels.is_empty() {
                labels.push(crate::names::format_label(
                    this.env(),
                    &append_combination(name),
                    codes[index].as_deref(),
                )?);
            }
            out.set(
                index,
                ProductProduct::DISPLAY_NAME,
                Some(Arc::<str>::from(labels.join(", "))),
            )?;
        }
        Ok(())
    }

    /// Code and partner_ref retain their own selection rules rather than borrowing display-name selection.
    #[compute(depends = [
        "name", "default_code", "display_name", "product_tmpl_id",
        "product_tmpl_id.seller_ids.product_name", "product_tmpl_id.seller_ids.product_code",
        "product_tmpl_id.seller_ids.partner_id", "product_tmpl_id.seller_ids.company_id",
        "product_tmpl_id.seller_ids.product_id", "product_tmpl_id.seller_ids.sequence",
        "product_tmpl_id.seller_ids.min_qty", "product_tmpl_id.seller_ids.price"
    ], depends_context = ["partner_id", "seller_id", "company_id", "display_default_code", "formatted_display_name", "lang", "label_reader"])]
    async fn _compute_partner_reference(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let names = crate::names::template_names(this, tx).await?;
        let codes = this.mapped(tx, ProductProduct::DEFAULT_CODE).await?;
        let labels = this.mapped(tx, ProductProduct::DISPLAY_NAME).await?;
        let templates = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
        let ids: Vec<i64> = templates
            .iter()
            .flatten()
            .filter(|id| !id.is_new())
            .map(|id| id.get())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let sellers = crate::names::sellers(this.env(), tx, &ids).await?;
        let partner = crate::names::context_id(this.env(), "partner_id")?;
        for index in 0..this.len() {
            let variant = this.as_records().rec_ids()[index].get();
            let template = templates[index]
                .ok_or_else(|| Error::validation("Product variant missing template link"))?
                .get();
            let matching = || {
                sellers
                    .iter()
                    .filter(|row| row.template == template && Some(row.partner) == partner)
            };
            let mut code = codes[index].clone();
            for row in matching().filter(|row| row.variant.is_none_or(|id| id == variant)) {
                code = row.code.clone().or_else(|| codes[index].clone());
                if row.variant == Some(variant) {
                    break;
                }
            }
            let reference = match matching().next() {
                Some(row) => {
                    let name = row
                        .name
                        .as_deref()
                        .or(codes[index].as_deref())
                        .or(names[index].as_deref())
                        .unwrap_or("");
                    match code.as_deref().filter(|v| !v.is_empty()) {
                        Some(code) => Some(Arc::<str>::from(format!("[{code}] {name}"))),
                        None => Some(Arc::<str>::from(name)),
                    }
                }
                None => labels[index].clone(),
            };
            out.set(index, ProductProduct::CODE, code)?;
            out.set(index, ProductProduct::PARTNER_REF, reference)?;
        }
        Ok(())
    }

    /// Compute catalog sales price: template.list_price + price_extra. It is
    /// stored, so it depends on the template `list_price` (delegated field) and `price_extra`.
    #[compute(depends = ["list_price", "price_extra"])]
    async fn _compute_lst_price(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let tmpl_model = this.env().records("product.template")?;
        let tmpl_ids = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
        let extras = this.mapped(tx, ProductProduct::PRICE_EXTRA).await?;

        for (index, (tmpl_opt, extra)) in tmpl_ids.into_iter().zip(extras).enumerate() {
            let tmpl_id = tmpl_opt
                .ok_or_else(|| Error::validation("Product variant missing template link"))?;
            let tmpl = tmpl_model.browse_ids([tmpl_id]);
            let base_price = match tmpl.cell(tx, "list_price").await? {
                Cell::Decimal(d) => d,
                // A template that is itself new (first onchange of a new variant) has no price yet.
                Cell::Null if tmpl_id.is_new() => Decimal::ZERO,
                other => {
                    return Err(Error::programming(format!(
                        "product.template.list_price returned {other:?}, expected a decimal"
                    )));
                }
            };
            out.set(index, ProductProduct::LST_PRICE, base_price + extra)?;
        }
        Ok(())
    }

    /// Inverse lst_price writes back to template list_price for single variant.
    #[method]
    async fn _set_lst_price(this: &RecordSet<ProductProduct>, tx: &mut Transaction) -> Result<()> {
        let tmpl_model = this.env().records("product.template")?;
        let prices = this.mapped(tx, ProductProduct::LST_PRICE).await?;
        let tmpl_ids = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;

        for (tmpl_opt, lst_price) in tmpl_ids.into_iter().zip(prices) {
            if let Some(tmpl_id) = tmpl_opt {
                let domain = Domain::condition("product_tmpl_id", Operator::Eq, tmpl_id.get())?;
                let variants = this.search(tx, &domain).await?;
                if variants.len() == 1 {
                    let tmpl = tmpl_model.browse_ids([tmpl_id]);
                    let mut vals = RawVals::new();
                    vals.insert("list_price".into(), Cell::Decimal(lst_price));
                    tmpl.write(tx, vals).await?;
                }
            }
        }
        Ok(())
    }

    /// `image_1920`: the variant's own picture of that size, otherwise the template's.
    #[compute(depends = ["image_variant_1920", "product_tmpl_id.image_1920"])]
    async fn _compute_image_1920(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let own = this.mapped(tx, ProductProduct::IMAGE_VARIANT_1920).await?;
        let shown = shown_pictures(this, tx, own, "image_1920").await?;
        for (index, picture) in shown.into_iter().enumerate() {
            out.set(index, ProductProduct::IMAGE_1920, picture)?;
        }
        Ok(())
    }

    /// `image_1024`: the variant's own picture of that size, otherwise the template's.
    #[compute(depends = ["image_variant_1024", "product_tmpl_id.image_1024"])]
    async fn _compute_image_1024(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let own = this.mapped(tx, ProductProduct::IMAGE_VARIANT_1024).await?;
        let shown = shown_pictures(this, tx, own, "image_1024").await?;
        for (index, picture) in shown.into_iter().enumerate() {
            out.set(index, ProductProduct::IMAGE_1024, picture)?;
        }
        Ok(())
    }

    /// `image_512`: the variant's own picture of that size, otherwise the template's.
    #[compute(depends = ["image_variant_512", "product_tmpl_id.image_512"])]
    async fn _compute_image_512(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let own = this.mapped(tx, ProductProduct::IMAGE_VARIANT_512).await?;
        let shown = shown_pictures(this, tx, own, "image_512").await?;
        for (index, picture) in shown.into_iter().enumerate() {
            out.set(index, ProductProduct::IMAGE_512, picture)?;
        }
        Ok(())
    }

    /// `image_256`: the variant's own picture of that size, otherwise the template's.
    #[compute(depends = ["image_variant_256", "product_tmpl_id.image_256"])]
    async fn _compute_image_256(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let own = this.mapped(tx, ProductProduct::IMAGE_VARIANT_256).await?;
        let shown = shown_pictures(this, tx, own, "image_256").await?;
        for (index, picture) in shown.into_iter().enumerate() {
            out.set(index, ProductProduct::IMAGE_256, picture)?;
        }
        Ok(())
    }

    /// `image_128`: the variant's own picture of that size, otherwise the template's.
    #[compute(depends = ["image_variant_128", "product_tmpl_id.image_128"])]
    async fn _compute_image_128(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let own = this.mapped(tx, ProductProduct::IMAGE_VARIANT_128).await?;
        let shown = shown_pictures(this, tx, own, "image_128").await?;
        for (index, picture) in shown.into_iter().enumerate() {
            out.set(index, ProductProduct::IMAGE_128, picture)?;
        }
        Ok(())
    }

    /// Inverse of `image_1920`: where a picture written on a variant is kept. Behaviour observed
    /// and `:287-288`.
    ///
    /// For each variant the value goes to the template's `image_1920`, and the variant's own
    /// picture is cleared, when
    /// - the value is empty and the variant has no own picture, or
    /// - the value is a picture and the template has none, or
    /// - the template has at most one active variant.
    ///
    /// Otherwise the value becomes the variant's own picture (`image_variant_1920`), and an empty
    /// value clears it so the variant shows the template's picture again. The written
    /// content is checked on this field before the inverse runs, so a refusal names
    /// `product.product.image_1920`.
    #[method]
    async fn _set_image_1920(this: &RecordSet<ProductProduct>, tx: &mut Transaction) -> Result<()> {
        // every written value is read before the first write: a write on a template drops the
        // values held for its variants
        let values = this.mapped(tx, ProductProduct::IMAGE_1920).await?;
        let templates = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
        let template_model = this.env().records("product.template")?;
        for (index, (value, template)) in values.into_iter().zip(templates).enumerate() {
            let variant = this.at(index as isize)?;
            let template_id = template
                .ok_or_else(|| Error::validation("Product variant missing template link"))?;
            let template = template_model.browse_ids([template_id]);
            // read per variant, after the writes of the variants before it: two variants of one
            // template without a picture give the first picture to the template and keep the
            // second on its variant
            let own = variant
                .mapped(tx, ProductProduct::IMAGE_VARIANT_1920)
                .await?
                .into_iter()
                .next()
                .flatten();
            let clears_nothing = value.is_none() && own.is_none();
            // emptiness from the small stored size, not the full-size content
            let fills_template =
                value.is_some() && picture(&template, tx, "image_128").await?.is_none();
            let to_template = clears_nothing || fills_template || {
                let active = Domain::condition("product_tmpl_id", Operator::Eq, template_id.get())?
                    & Domain::condition("active", Operator::Eq, true)?;
                this.search_count(tx, &active, None).await? <= 1
            };
            let cell = value.map_or(Cell::Null, Cell::Bytes);
            if to_template {
                if own.is_some() {
                    variant
                        .as_records()
                        .write(
                            tx,
                            RawVals::new().set_cell("image_variant_1920", Cell::Null),
                        )
                        .await?;
                }
                template
                    .write(tx, RawVals::new().set_cell("image_1920", cell))
                    .await?;
            } else {
                variant
                    .as_records()
                    .write(tx, RawVals::new().set_cell("image_variant_1920", cell))
                    .await?;
            }
        }
        // the written value is held for the shown picture while the inverse runs; drop it, so the
        // next read in this transaction returns what was stored (scaled and encoded again), from
        // the variant or from the template
        tx.invalidate_records(this.as_records(), Some(&["image_1920"]))
            .await?;
        Ok(())
    }

    /// Compute price_extra as sum of PTAV extra prices.
    #[compute(depends = ["product_template_attribute_value_ids"])]
    async fn _compute_price_extra(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        let ptav_model = this.env().records("product.template.attribute.value")?;
        for index in 0..this.len() {
            let rec = this.at(index as isize)?;
            // record ids, not raw ids: in an onchange the values are in-memory records
            let ptav_ids: Vec<RecId> = rec
                .mapped(tx, ProductProduct::PRODUCT_TEMPLATE_ATTRIBUTE_VALUE_IDS)
                .await?
                .into_iter()
                .flatten()
                .collect();
            let total_extra = if ptav_ids.is_empty() {
                Decimal::ZERO
            } else {
                let ptav_set = RecordSet::<ProductTemplateAttributeValue>::try_from(
                    ptav_model.browse_ids(ptav_ids),
                )?;
                ptav_set
                    .mapped(tx, ProductTemplateAttributeValue::PRICE_EXTRA)
                    .await?
                    .into_iter()
                    .sum()
            };
            out.set(index, ProductProduct::PRICE_EXTRA, total_extra)?;
        }
        Ok(())
    }

    /// Compute combination_indices serialized string.
    #[compute(depends = ["product_template_attribute_value_ids"])]
    async fn _compute_combination_indices(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        out: &mut Assign<'_, ProductProduct>,
    ) -> Result<()> {
        for index in 0..this.len() {
            let rec = this.at(index as isize)?;
            let ptav_ids: Vec<RecId> = rec
                .mapped(tx, ProductProduct::PRODUCT_TEMPLATE_ATTRIBUTE_VALUE_IDS)
                .await?
                .into_iter()
                .flatten()
                .collect();
            if ptav_ids.is_empty() {
                out.set(index, ProductProduct::COMBINATION_INDICES, None)?;
            } else {
                // the stored rows, also for the in-memory values of a form's onchange
                let mut sorted_ids: Vec<i64> = ptav_ids
                    .into_iter()
                    .filter_map(|id| id.as_real().or(id.origin()))
                    .map(|id| id.get())
                    .collect();
                sorted_ids.sort_unstable();
                let serialized = sorted_ids
                    .into_iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<String>>()
                    .join(",");
                out.set(
                    index,
                    ProductProduct::COMBINATION_INDICES,
                    Some(serialized.into()),
                )?;
            }
        }
        Ok(())
    }

    #[method(override)]
    async fn create(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        vals_list: Vec<RawVals>,
        sup: Super,
    ) -> Result<Records> {
        tx.savepoint(async |tx| {
            // Pre-lock only already-resolved parent IDs. Super validates/defaults the full values;
            // final reconciliation uses every actual stored parent, including delegated creation.
            let families: Vec<i64> = vals_list
                .iter()
                .filter_map(|v| match v.get("product_tmpl_id") {
                    Some(Cell::Id(id)) => Some(*id),
                    _ => None,
                })
                .collect();
            let keys = crate::barcode::prepare(tx, this.env(), &families, &[], &vals_list).await?;
            let mirrored = crate::barcode::single_mirrors(tx, &families).await?;
            let records: Records = sup.call(tx, this.as_records(), (vals_list,)).await?;
            crate::barcode::drop_stale_mirrors(tx, &mirrored).await?;
            crate::packaging_lifecycle::register_units(
                &records,
                tx,
                &["uom_id", "uom_ids", "extra_uom_ids"],
            )
            .await?;
            let ids: Vec<i64> = records.rec_ids().iter().map(|id| id.get()).collect();
            let families = crate::barcode::variant_families(tx, &ids).await?;
            crate::barcode::reconcile(tx, this.env(), &families, &[], keys).await?;
            Ok(records)
        })
        .await
    }

    /// Release or reclassify claims even when deleting the last mirrored variant. A variant's own
    /// picture moves to a template that has none before the variant goes
    /// (`keep_pictures_on_templates`).
    #[method(override)]
    async fn unlink(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        sup: Super,
    ) -> Result<()> {
        crate::barcode::refuse_bypass(this.env())?;
        // one savepoint: a delete that is refused after the picture moved leaves the template as
        // it was, also for a caller that handles the error and goes on
        tx.savepoint(async |tx| {
            let ids: Vec<i64> = this
                .as_records()
                .rec_ids()
                .iter()
                .map(|id| id.get())
                .collect();
            let families = crate::barcode::variant_families(tx, &ids).await?;
            let keys = crate::barcode::prepare(tx, this.env(), &families, &[], &[]).await?;
            // the right to delete is settled first: the move below reads the variants, and a
            // caller who may not delete them is refused for the delete, not for a read
            this.as_records()
                .check_access(tx, erp_sdk::Operation::Unlink)
                .await?;
            keep_pictures_on_templates(this, tx).await?;
            sup.call(tx, this.as_records(), ()).await?;
            crate::barcode::reconcile(tx, this.env(), &families, &[], keys).await
        })
        .await
    }

    #[method(override)]
    async fn write(
        this: &RecordSet<ProductProduct>,
        tx: &mut Transaction,
        vals: RawVals,
        sup: Super,
    ) -> Result<()> {
        tx.savepoint(async |tx| {
            crate::barcode::refuse_bypass(this.env())?;
            let ids: Vec<i64> = this
                .as_records()
                .rec_ids()
                .iter()
                .map(|id| id.get())
                .collect();
            let mut families = crate::barcode::variant_families(tx, &ids).await?;
            if let Some(Cell::Id(id)) = vals.get("product_tmpl_id") {
                families.push(*id);
            }
            let relevant = vals.contains("barcode")
                || vals.contains("company_id")
                || vals.contains("product_tmpl_id");
            let keys = if relevant {
                crate::barcode::prepare(tx, this.env(), &families, &[], std::slice::from_ref(&vals))
                    .await?
            } else {
                Vec::new()
            };
            if let Some(Cell::Bool(false)) = vals.get("active") {
                let combo_item_model = this.env().sudo(true).records("product.combo.item")?;
                let ids: Vec<i64> = this
                    .as_records()
                    .rec_ids()
                    .iter()
                    .map(|id| id.get())
                    .collect();
                let d = Domain::condition("product_id", Operator::In, ids)?;
                if combo_item_model.search_count(tx, &d, None).await? > 0 {
                    return Err(Error::validation(
                        "This product belongs to a combo, so it cannot be archived. Remove it from its combos first.",
                    ));
                }
            }

            let units_changed = vals.contains("uom_id") || vals.contains("uom_ids") || vals.commands("uom_ids").is_some() || vals.contains("extra_uom_ids") || vals.commands("extra_uom_ids").is_some() || vals.contains("product_tmpl_id");
            // A variant moved into a single-variant family ends that family's mirror.
            let mirrored = match vals.get("product_tmpl_id") {
                Some(Cell::Id(id)) => crate::barcode::single_mirrors(tx, &[*id]).await?,
                _ => Vec::new(),
            };
            sup.call(tx, this.as_records(), (vals,)).await?;
            crate::barcode::drop_stale_mirrors(tx, &mirrored).await?;
            if units_changed {
                crate::packaging_lifecycle::register_units(this.as_records(), tx, &["uom_id", "uom_ids", "extra_uom_ids"]).await?;
            }
            if relevant {
                crate::barcode::reconcile(tx, this.env(), &families, &[], keys).await?;
            }
            Ok(())
        })
        .await
    }
}

/// The picture a record holds in `field`, read by name: `None` when the field is empty.
async fn picture(record: &Records, tx: &mut Transaction, field: &str) -> Result<Option<Blob>> {
    let cell = record.cell(tx, field).await?;
    picture_cell(record, field, cell)
}

/// A picture cell of `record.field`: `None` when empty, an error for any other kind of value.
fn picture_cell(record: &Records, field: &str, cell: Cell) -> Result<Option<Blob>> {
    match cell {
        Cell::Bytes(content) => Ok(Some(content)),
        Cell::Null => Ok(None),
        other => Err(Error::programming(format!(
            "{}.{field} returned {other:?}, expected picture content",
            record.model_name()
        ))),
    }
}

/// The picture each variant shows in one size: its own (`own`, one entry per record of `this`),
/// otherwise the template's `template_field`. A variant that has no template yet (a draft) shows
/// its own picture only. One entry per record, in the order of the set.
async fn shown_pictures(
    this: &RecordSet<ProductProduct>,
    tx: &mut Transaction,
    own: Vec<Option<Blob>>,
    template_field: &str,
) -> Result<Vec<Option<Blob>>> {
    let templates = this.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
    let template_model = this.env().records("product.template")?;
    // the templates of the variants without an own picture, read in one statement
    let needed: BTreeSet<RecId> = own
        .iter()
        .zip(&templates)
        .filter_map(|(own, template)| if own.is_none() { *template } else { None })
        .collect();
    let needed = template_model.browse_ids(needed);
    let cells = needed.cells(tx, template_field).await?;
    let mut by_template: HashMap<RecId, Option<Blob>> = HashMap::with_capacity(cells.len());
    for (template, cell) in needed.rec_ids().iter().zip(cells) {
        by_template.insert(*template, picture_cell(&needed, template_field, cell)?);
    }
    Ok(own
        .into_iter()
        .zip(templates)
        .map(|(own, template)| {
            own.or_else(|| template.and_then(|t| by_template.get(&t).cloned().flatten()))
        })
        .collect())
}

/// Before variants are deleted: a variant's own picture moves to its template when the template
/// has none, so the picture is not lost with the variant (behaviour observed in
/// ). Records already deleted are skipped.
async fn keep_pictures_on_templates(
    this: &RecordSet<ProductProduct>,
    tx: &mut Transaction,
) -> Result<()> {
    let existing = this.exists(tx).await?;
    // presence is decided on the small stored sizes, which are empty exactly when the full-size
    // picture is; full-size content is read only for the variant that fills a template
    let has_own = existing
        .mapped(tx, ProductProduct::IMAGE_VARIANT_128)
        .await?;
    let templates = existing.mapped(tx, ProductProduct::PRODUCT_TMPL_ID).await?;
    let template_model = this.env().records("product.template")?;
    let candidates: BTreeSet<RecId> = has_own
        .iter()
        .zip(&templates)
        .filter_map(|(own, template)| if own.is_some() { *template } else { None })
        .collect();
    let candidates = template_model.browse_ids(candidates);
    let cells = candidates.cells(tx, "image_128").await?;
    let mut without_picture = BTreeSet::new();
    for (template, cell) in candidates.rec_ids().iter().zip(cells) {
        if picture_cell(&candidates, "image_128", cell)?.is_none() {
            without_picture.insert(*template);
        }
    }
    let ids = existing.as_records().rec_ids();
    for ((own, template), id) in has_own.into_iter().zip(templates).zip(ids) {
        let (Some(_), Some(template)) = (own, template) else {
            continue;
        };
        // the first variant with a picture fills the template
        if !without_picture.remove(&template) {
            continue;
        }
        // a set of this one variant: a read on `existing` would prefetch every full-size picture
        let variant = existing.as_records().browse_ids([*id]);
        let Some(content) = picture(&variant, tx, "image_variant_1920").await? else {
            continue;
        };
        template_model
            .browse_ids([template])
            .write(
                tx,
                RawVals::new().set_cell("image_1920", Cell::Bytes(content)),
            )
            .await?;
    }
    Ok(())
}

/// Order of a value inside a variant name: attribute (sequence, id), then value (sequence, id).
type ValueOrder = (i32, i64, i32, i64);
/// A value that belongs in a variant name, with its place.
type ShownValue = (ValueOrder, Arc<str>);

/// The attribute part of each variant's name: the names of the variant's
/// attribute values joined with ", ", ordered by attribute (sequence, id) then value (sequence,
/// id). A value is left out when its attribute creates no variants or when its line on the
/// template holds a single value, since every variant shares it. One entry per record, in the
/// order of the set; an empty string for a variant with nothing to show.
async fn combination_names(
    this: &RecordSet<ProductProduct>,
    tx: &mut Transaction,
) -> Result<Vec<String>> {
    let per_variant = this
        .mapped(tx, ProductProduct::PRODUCT_TEMPLATE_ATTRIBUTE_VALUE_IDS)
        .await?;
    // record ids, not raw ids: in an onchange the values are in-memory records
    let ptav_ids: Vec<RecId> = per_variant
        .iter()
        .flatten()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if ptav_ids.is_empty() {
        return Ok(vec![String::new(); per_variant.len()]);
    }

    let env = this.env();
    let ptavs = RecordSet::<ProductTemplateAttributeValue>::try_from(
        env.records("product.template.attribute.value")?
            .browse_ids(ptav_ids.iter().copied()),
    )?;
    let lines = ptavs
        .mapped(tx, ProductTemplateAttributeValue::ATTRIBUTE_LINE_ID)
        .await?;
    let raw_values = ptavs
        .mapped(
            tx,
            ProductTemplateAttributeValue::PRODUCT_ATTRIBUTE_VALUE_ID,
        )
        .await?;

    let line_ids: Vec<i64> = lines
        .iter()
        .flatten()
        .map(|id| id.get())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let line_set = RecordSet::<ProductTemplateAttributeLine>::try_from(
        env.records("product.template.attribute.line")?
            .browse(line_ids.clone())?,
    )?;
    let line_attrs = line_set
        .mapped(tx, ProductTemplateAttributeLine::ATTRIBUTE_ID)
        .await?;
    let line_values = line_set
        .mapped(tx, ProductTemplateAttributeLine::PRODUCT_TEMPLATE_VALUE_IDS)
        .await?;

    let attr_ids: Vec<i64> = line_attrs
        .iter()
        .flatten()
        .map(|id| id.get())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let attr_set = RecordSet::<ProductAttribute>::try_from(
        env.records("product.attribute")?.browse(attr_ids.clone())?,
    )?;
    let attr_modes = attr_set
        .mapped(tx, ProductAttribute::CREATE_VARIANT)
        .await?;
    let attr_sequences = attr_set.mapped(tx, ProductAttribute::SEQUENCE).await?;
    let attributes: HashMap<i64, (bool, i32)> = attr_ids
        .iter()
        .zip(attr_modes.into_iter().zip(attr_sequences))
        .map(|(id, (mode, sequence))| (*id, (mode.as_deref() != Some("no_variant"), sequence)))
        .collect();
    // line id -> (attribute id, the line holds more than one value)
    let mut line_info: HashMap<i64, (i64, bool)> = HashMap::new();
    for ((line, attr), values) in line_ids.iter().zip(line_attrs).zip(line_values) {
        let attr = attr.ok_or_else(|| {
            Error::programming(format!(
                "product.template.attribute.line {line} has no attribute"
            ))
        })?;
        line_info.insert(*line, (attr.get(), values.len() > 1));
    }

    let value_ids: Vec<i64> = raw_values
        .iter()
        .flatten()
        .map(|id| id.get())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let value_set = RecordSet::<ProductAttributeValue>::try_from(
        env.records("product.attribute.value")?
            .browse(value_ids.clone())?,
    )?;
    let value_names = value_set.mapped(tx, ProductAttributeValue::NAME).await?;
    let value_sequences = value_set
        .mapped(tx, ProductAttributeValue::SEQUENCE)
        .await?;
    let values: HashMap<i64, (Arc<str>, i32)> = value_ids
        .iter()
        .zip(value_names.into_iter().zip(value_sequences))
        .map(|(id, (name, sequence))| (*id, (name.unwrap_or_else(|| Arc::from("")), sequence)))
        .collect();

    // ptav id -> sort key and name, only for the values that belong in the name
    let mut shown: HashMap<RecId, ShownValue> = HashMap::new();
    for ((ptav, line), value) in ptav_ids.iter().zip(lines).zip(raw_values) {
        let (Some(line), Some(value)) = (line, value) else {
            return Err(Error::programming(format!(
                "product.template.attribute.value {ptav} lacks its line or value"
            )));
        };
        let (attr, several) = line_info[&line.get()];
        let (creates_variants, attr_sequence) = attributes[&attr];
        if !creates_variants || !several {
            continue;
        }
        let (name, value_sequence) = values[&value.get()].clone();
        shown.insert(
            *ptav,
            ((attr_sequence, attr, value_sequence, value.get()), name),
        );
    }

    Ok(per_variant
        .iter()
        .map(|ids| {
            let mut parts: Vec<&ShownValue> = ids.iter().filter_map(|id| shown.get(id)).collect();
            parts.sort_by_key(|(key, _)| *key);
            parts
                .iter()
                .map(|(_, name)| name.as_ref())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect())
}
