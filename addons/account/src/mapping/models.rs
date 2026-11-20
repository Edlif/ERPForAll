// SPDX-License-Identifier: MIT OR Apache-2.0
//! Database declarations for the exact-company default roles.

use erp_sdk::prelude::*;

#[model(
    name = "account.default.role",
    description = "Company Default Account Role",
    rec_name = "key",
    constraints = [
        ("_key", "UNIQUE (key)", "Each company default account role has one declaration."),
        ("_registered_key", "CHECK (key IN ('receivable', 'payable', 'income', 'expense', 'stock_valuation'))", "The company default account role must be declared.")
    ],
)]
struct DefaultRoleDeclaration {
    #[field(char, required = true, readonly = true)]
    key: Option<Arc<str>>,
}

#[model(
    name = "account.default.generation",
    description = "Company Default Account Generation",
    rec_name = "company_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_company", "UNIQUE (company_id)", "Each company has one accounting configuration generation."),
        ("_nonnegative", "CHECK (generation >= 0)", "Accounting configuration generations cannot be negative.")
    ],
)]
struct DefaultGeneration {
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        readonly = true
    )]
    company_id: Option<RecId>,
    #[field(bigint, required = true, readonly = true)]
    generation: erp_sdk::BigInt,
    // The full xid distinguishes writes in this transaction even across an ORM savepoint.
    #[field(xid8, readonly = true)]
    writer_xid: Option<erp_sdk::Xid8>,
}

#[model(
    name = "account.default.key",
    description = "Company Default Account Key",
    rec_name = "role_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_role_company", "UNIQUE (company_id, role_id)", "Each company has one default account key for each role."),
        ("_id_company", "UNIQUE (id, company_id)", "The default account key belongs to one company."),
        ("_version", "CHECK (version > 0)", "The default account key version must be positive.")
    ],
    indexes = [index(name = "_role", fields = ["role_id"])],
)]
struct DefaultKey {
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        readonly = true
    )]
    company_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "account.default.role",
        required = true,
        ondelete = "restrict",
        readonly = true
    )]
    role_id: Option<RecId>,
    #[field(bigint, required = true, readonly = true)]
    version: erp_sdk::BigInt,
}

#[model(
    name = "account.default.segment",
    description = "Company Default Account Revision",
    rec_name = "key_id",
    company_access = direct(field = "company_id", branch = "exact", empty = "deny"),
    constraints = [
        ("_key_company", "FOREIGN KEY (key_id, company_id) REFERENCES account_default_key (id, company_id) ON DELETE RESTRICT", "The default account revision must have its key's company."),
        ("_start_generation", "UNIQUE (key_id, effective_from, generation)", "Duplicate accounting mapping segment"),
        ("_generation", "CHECK (generation > 0)", "The default account revision generation must be positive."),
        ("_state_account", "CHECK ((state = 'account' AND account_id IS NOT NULL) OR (state = 'empty' AND account_id IS NULL))", "The stored account must agree with the company default revision state.")
    ],
    indexes = [index(name = "_company", fields = ["company_id"])],
)]
struct DefaultSegment {
    #[field(
        many2one,
        comodel = "account.default.key",
        required = true,
        ondelete = "restrict",
        readonly = true
    )]
    key_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "res.company",
        required = true,
        ondelete = "restrict",
        readonly = true
    )]
    company_id: Option<RecId>,
    #[field(
        many2one,
        comodel = "account.account",
        index = true,
        ondelete = "restrict",
        readonly = true
    )]
    account_id: Option<RecId>,
    #[field(selection, selection = [("account", "Account"), ("empty", "Empty")], required = true, readonly = true)]
    state: Option<Arc<str>>,
    #[field(date, required = true, readonly = true)]
    effective_from: Option<Date>,
    #[field(bigint, required = true, readonly = true)]
    generation: erp_sdk::BigInt,
}
