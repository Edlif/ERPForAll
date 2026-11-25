// SPDX-License-Identifier: MIT OR Apache-2.0
//! Send Quotation by Email (`sale.quotation.send`) and Customer Online Signature & Confirmation
//! (`sale.order.sign.wizard`).

use erp_addon_mail::{Body, PostRequest, PostSubtype, PostType, Posted};
use erp_sdk::prelude::*;
use erp_sdk::serde_json::json;
use erp_sdk::{Operation, ValueMap};

use crate::order::SaleOrder;

/// `sale.quotation.send`: transient email composer for sending quotations and pro-forma invoices.
#[model(
    name = "sale.quotation.send",
    description = "Send Quotation by Email",
    kind = "transient",
    transient_max_age_seconds = 3600,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct SaleQuotationSend {
    /// Order being sent.
    #[field(
        many2one,
        comodel = "sale.order",
        required = true,
        ondelete = "cascade",
        string = "Quotation / Order"
    )]
    pub order_id: Option<RecId>,
    /// Company from the order.
    #[field(
        many2one,
        comodel = "res.company",
        related = "order_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Recipient partner.
    #[field(many2one, comodel = "res.partner", string = "Recipient")]
    pub partner_id: Option<RecId>,
    /// Recipient email address.
    #[field(char, string = "Email To")]
    pub email_to: Option<Arc<str>>,
    /// Email subject.
    #[field(char, required = true, string = "Subject")]
    pub subject: Option<Arc<str>>,
    /// Attach pro-forma invoice instead of standard quotation.
    #[field(boolean, default = false, string = "Send as Pro-Forma Invoice")]
    pub is_proforma: bool,
    /// Email body.
    #[field(text, string = "Message")]
    pub body: Option<Arc<str>>,
}

#[methods(SaleQuotationSend, addon = "sale")]
impl SaleQuotationSend {
    #[method(override)]
    async fn default_get(
        this: &RecordSet<SaleQuotationSend>,
        tx: &mut Transaction,
        names: Vec<String>,
        sup: Super,
    ) -> Result<ValueMap> {
        let mut values: ValueMap = sup.call(tx, this.as_records(), (names.clone(),)).await?;
        let oid = match this.env().context().get("default_order_id") {
            Some(Value::Int(id)) => Some(id),
            _ => None,
        };
        if let Some(order_id) = oid {
            let order = this.env().records("sale.order")?.browse([order_id])?;
            let oname = match order.cell(tx, "name").await {
                Ok(Cell::Str(s)) => s.to_string(),
                _ => format!("Order {order_id}"),
            };
            let total = match order.cell(tx, "amount_total").await {
                Ok(Cell::Decimal(d)) => d.to_string(),
                _ => "0.00".to_owned(),
            };
            let partner_id = match order.cell(tx, "partner_id").await {
                Ok(Cell::Id(pid)) => Some(pid),
                _ => None,
            };
            if names.iter().any(|n| n == "partner_id")
                && !values.contains_key("partner_id")
                && let Some(pid) = partner_id
            {
                values.insert("partner_id".to_owned(), Value::Int(pid));
            }
            if names.iter().any(|n| n == "email_to")
                && !values.contains_key("email_to")
                && let Some(pid) = partner_id
                && let Ok(Cell::Str(email)) = this
                    .env()
                    .records("res.partner")?
                    .browse([pid])?
                    .cell(tx, "email")
                    .await
            {
                values.insert("email_to".to_owned(), Value::Str(email));
            }
            if names.iter().any(|n| n == "subject") && !values.contains_key("subject") {
                values.insert(
                    "subject".to_owned(),
                    Value::Str(Arc::from(format!("Quotation {oname}"))),
                );
            }
            if names.iter().any(|n| n == "body") && !values.contains_key("body") {
                values.insert(
                    "body".to_owned(),
                    Value::Str(Arc::from(format!(
                        "Hello,\n\nPlease find attached your quotation {oname} totaling {total}.\n\nBest regards,"
                    ))),
                );
            }
        }
        Ok(values)
    }

    /// Logs the email in the order chatter, marks a draft quotation as Sent, and closes the modal.
    #[method]
    #[rpc(effect = write)]
    pub async fn action_send_mail(
        this: &RecordSet<SaleQuotationSend>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let wizard = this.ensure_one()?;
        wizard
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let order_id = wizard
            .get(tx, SaleQuotationSend::ORDER_ID)
            .await?
            .ok_or_else(|| Error::validation("Choose a quotation to send."))?
            .get();
        let email_to = wizard
            .get(tx, SaleQuotationSend::EMAIL_TO)
            .await?
            .unwrap_or_else(|| Arc::from("customer"));
        let subject = wizard
            .get(tx, SaleQuotationSend::SUBJECT)
            .await?
            .unwrap_or_else(|| Arc::from("Quotation"));
        let is_proforma = wizard.get(tx, SaleQuotationSend::IS_PROFORMA).await?;
        let body = wizard
            .get(tx, SaleQuotationSend::BODY)
            .await?
            .unwrap_or_default();

        let env = wizard
            .env()
            .with_context_replaced(erp_sdk::Context::default());
        let order = env.records("sale.order")?.browse([order_id])?;
        order.check_access(tx, Operation::Write).await?;
        let state = order.cell(tx, "state").await?;
        if state == Cell::Str("draft".into()) {
            let _: () = order.call(tx, "action_quotation_sent", ()).await?;
        }
        let doc_kind = if is_proforma {
            "Pro-Forma Invoice"
        } else {
            "Quotation"
        };
        let note_body = format!("{doc_kind} emailed to {email_to}\n\n{body}");
        let mut req = PostRequest::new(Body::Text(note_body));
        req.subject = Some(subject.to_string());
        req.message_type = PostType::Comment;
        req.subtype = PostSubtype::Comment;
        let _: Posted = order.call(tx, "message_post", (req,)).await?;

        wizard.as_records().unlink(tx).await?;
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": format!("{doc_kind} sent to {email_to}."),
                "type": "success",
                "next": {"type": "ir.actions.act_window_close"},
            },
        }))))
    }
}

/// `sale.order.sign.wizard`: online customer signature & portal acceptance dialog.
#[model(
    name = "sale.order.sign.wizard",
    description = "Customer Signature & Acceptance",
    kind = "transient",
    transient_max_age_seconds = 3600,
    company_access = direct(field = "company_id", branch = "exact", empty = "deny")
)]
pub struct SaleOrderSignWizard {
    /// Quotation being signed.
    #[field(
        many2one,
        comodel = "sale.order",
        required = true,
        ondelete = "cascade",
        string = "Quotation"
    )]
    pub order_id: Option<RecId>,
    /// Company from the quotation.
    #[field(
        many2one,
        comodel = "res.company",
        related = "order_id.company_id",
        store = true,
        string = "Company"
    )]
    pub company_id: Option<RecId>,
    /// Currency from the quotation.
    #[field(
        many2one,
        comodel = "res.currency",
        related = "order_id.currency_id",
        store = true,
        string = "Currency"
    )]
    pub currency_id: Option<RecId>,
    /// Total amount being accepted.
    #[field(
        monetary,
        currency_field = "currency_id",
        related = "order_id.amount_total",
        string = "Order Total"
    )]
    pub amount_total: Decimal,
    /// Full name of the signing customer representative.
    #[field(char, required = true, string = "Signed By")]
    pub signed_by: Option<Arc<str>>,
    /// Customer confirms agreement with quotation terms.
    #[field(boolean, default = true, string = "I accept the quotation and terms")]
    pub accept_terms: bool,
    /// Optional online payment confirmation reference.
    #[field(char, string = "Online Payment Reference")]
    pub payment_reference: Option<Arc<str>>,
}

#[methods(SaleOrderSignWizard, addon = "sale")]
impl SaleOrderSignWizard {
    #[method]
    #[rpc(effect = write)]
    pub async fn action_sign_and_confirm(
        this: &RecordSet<SaleOrderSignWizard>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let wizard = this.ensure_one()?;
        wizard
            .as_records()
            .check_access(tx, Operation::Write)
            .await?;
        let order_id = wizard
            .get(tx, SaleOrderSignWizard::ORDER_ID)
            .await?
            .ok_or_else(|| Error::validation("Choose a quotation to sign."))?
            .get();
        let signer = wizard
            .get(tx, SaleOrderSignWizard::SIGNED_BY)
            .await?
            .unwrap_or_default();
        let signer = signer.trim();
        if signer.is_empty() {
            return Err(Error::user("Enter the signer's full name."));
        }
        let accepted = wizard.get(tx, SaleOrderSignWizard::ACCEPT_TERMS).await?;
        if !accepted {
            return Err(Error::user(
                "Confirm acceptance of the quotation terms before signing.",
            ));
        }
        let pay_ref = wizard
            .get(tx, SaleOrderSignWizard::PAYMENT_REFERENCE)
            .await?;

        let env = wizard
            .env()
            .with_context_replaced(erp_sdk::Context::default());
        let order = env.records("sale.order")?.browse([order_id])?;
        order.check_access(tx, Operation::Write).await?;

        let now = DateTime::now();
        let mut vals = RawVals::new();
        vals.insert("signed_by".into(), Cell::Str(signer.into()));
        vals.insert("signed_on".into(), Cell::DateTime(now));
        order.write(tx, vals).await?;

        let state = order.cell(tx, "state").await?;
        if matches!(state, Cell::Str(ref s) if matches!(s.as_ref(), "draft" | "sent")) {
            let _: () = order.call(tx, "action_confirm", ()).await?;
        }
        let pay_note = pay_ref
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|r| format!(" (Payment ref: {r})"))
            .unwrap_or_default();
        let msg = format!("Quotation digitally signed and accepted by {signer}{pay_note}.");
        let req = PostRequest::new(Body::Text(msg));
        let _: Posted = order.call(tx, "message_post", (req,)).await?;

        wizard.as_records().unlink(tx).await?;
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.client",
            "tag": "display_notification",
            "params": {
                "message": format!("Order signed by {signer} and confirmed."),
                "type": "success",
                "next": {"type": "ir.actions.act_window_close"},
            },
        }))))
    }
}

#[methods(SaleOrder, addon = "sale")]
impl SaleOrder {
    /// Opens the Send Quotation by Email wizard.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_open_send_wizard(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let oid = rec.as_records().ids()[0];
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Send by Email",
            "res_model": "sale.quotation.send",
            "target": "new",
            "views": [[false, "form"]],
            "context": {"default_order_id": oid},
        }))))
    }

    /// Opens the Customer Online Signature & Acceptance dialog.
    #[method]
    #[rpc(effect = read)]
    pub async fn action_open_sign_wizard(
        this: &RecordSet<SaleOrder>,
        tx: &mut Transaction,
    ) -> Result<Value> {
        let rec = this.ensure_one()?;
        rec.as_records().check_access(tx, Operation::Write).await?;
        let oid = rec.as_records().ids()[0];
        Ok(Value::Json(Arc::new(json!({
            "type": "ir.actions.act_window",
            "name": "Customer Signature",
            "res_model": "sale.order.sign.wizard",
            "target": "new",
            "views": [[false, "form"]],
            "context": {"default_order_id": oid},
        }))))
    }
}
