// SPDX-License-Identifier: MIT OR Apache-2.0
//! The install-time default records of `account` and `stock`, as (query, expected rows): one
//! set of checks for the direct install tests and the served-node install test.
#![allow(
    dead_code,
    reason = "each test binary uses the checks of its own addons"
)]

/// The master account tags, the Accounting menus and the accounting groups.
pub const ACCOUNT: &[(&str, &[&str])] = &[
    (
        "SELECT d.name || '=' || t.name || '/' || t.applicability FROM ir_model_data d \
         JOIN account_account_tag t ON t.id = d.res_id \
         WHERE d.module = 'account' AND d.model = 'account.account.tag' ORDER BY d.name",
        &[
            "account_tag_financing=Financing Activities/accounts",
            "account_tag_investing=Investing & Extraordinary Activities/accounts",
            "account_tag_operating=Operating Activities/accounts",
        ],
    ),
    (
        "SELECT name FROM ir_model_data WHERE module = 'account' AND model = 'ir.ui.menu' ORDER BY name",
        &[
            "menu_account_fiscal_period",
            "menu_account_tag",
            "menu_action_account_form",
            "menu_action_account_journal_form",
            "menu_action_account_journal_group_list",
            "menu_action_account_moves_all",
            "menu_action_move_in_invoice_type",
            "menu_action_move_in_refund_type",
            "menu_action_move_journal_line",
            "menu_action_move_out_invoice_type",
            "menu_action_move_out_refund_type",
            "menu_board_journal_1",
            "menu_finance",
            "menu_finance_configuration",
            "menu_finance_entries",
            "menu_finance_payables",
            "menu_finance_receivables",
            "menu_tax_groups",
            "menu_taxes",
        ],
    ),
    (
        "SELECT name FROM ir_model_data WHERE module = 'account' AND model = 'res.groups' ORDER BY name",
        &[
            "group_account_manager",
            "group_account_readonly",
            "group_account_user",
        ],
    ),
    (
        "SELECT d.name || '=' || a.res_model || '/' || a.domain || '/' || a.context FROM ir_model_data d \
         JOIN ir_act_window a ON a.id = d.res_id WHERE d.module = 'account' \
         AND d.model = 'ir.actions.act_window' AND d.name LIKE 'action\\_move\\_%\\_type' ORDER BY d.name",
        &[
            "action_move_in_invoice_type=account.move/[('move_type', '=', 'in_invoice')]/{'default_move_type': 'in_invoice'}",
            "action_move_in_refund_type=account.move/[('move_type', '=', 'in_refund')]/{'default_move_type': 'in_refund'}",
            "action_move_out_invoice_type=account.move/[('move_type', '=', 'out_invoice')]/{'default_move_type': 'out_invoice'}",
            "action_move_out_refund_type=account.move/[('move_type', '=', 'out_refund')]/{'default_move_type': 'out_refund'}",
        ],
    ),
    (
        "SELECT d.name || '=' || v.model || '/' || v.type FROM ir_model_data d JOIN ir_ui_view v \
         ON v.id = d.res_id WHERE d.module = 'account' AND d.model = 'ir.ui.view' \
         AND d.name LIKE 'view\\_invoice\\_%' ORDER BY d.name",
        &[
            "view_invoice_form=account.move/form",
            "view_invoice_list=account.move/list",
            "view_invoice_search=account.move/search",
        ],
    ),
];

/// The default removal strategies, shared locations, the MTO route and the topology version row.
pub const STOCK: &[(&str, &[&str])] = &[
    (
        "SELECT d.name || '=' || r.method || '/' || (r.name->>'en_US') FROM ir_model_data d \
         JOIN product_removal r ON r.id = d.res_id \
         WHERE d.module = 'stock' AND d.model = 'product.removal' ORDER BY d.name",
        &[
            "removal_closest=closest/Closest Location",
            "removal_fifo=fifo/First In First Out (FIFO)",
            "removal_least_packages=least_packages/Least Packages",
            "removal_lifo=lifo/Last In First Out (LIFO)",
        ],
    ),
    (
        "SELECT d.name || '=' || l.usage || '/' || l.complete_name || '/' || l.active::text \
         || '/' || coalesce(l.company_id::text, 'shared') || '/' || count(p.id)::text \
         FROM ir_model_data d JOIN stock_location l ON l.id = d.res_id \
         LEFT JOIN stock_location_policy p ON p.location_id = l.id AND NOT p.forbid_negative \
         WHERE d.module = 'stock' AND d.model = 'stock.location' \
         GROUP BY d.name, l.id ORDER BY d.name",
        &[
            "stock_location_customers=customer/Customers/true/shared/1",
            "stock_location_inter_company=transit/Inter-company transit/false/shared/1",
            "stock_location_suppliers=supplier/Vendors/true/shared/1",
        ],
    ),
    (
        "SELECT d.name || '=' || (r.name->>'en_US') || '/' || r.active::text || '/' || \
         r.sequence::text || '/' || r.product_selectable::text || '/' || \
         r.product_categ_selectable::text || '/' || coalesce(r.company_id::text, 'shared') \
         FROM ir_model_data d JOIN stock_route r ON r.id = d.res_id \
         WHERE d.module = 'stock' AND d.model = 'stock.route' ORDER BY d.name",
        &["route_warehouse0_mto=Replenish on Order (MTO)/false/5/true/true/shared"],
    ),
    (
        "SELECT d.name || '=' || v.generation::text FROM stock_topology_version v \
         LEFT JOIN ir_model_data d ON d.res_id = v.id AND d.model = 'stock.topology.version'",
        &["stock_topology_version=0"],
    ),
];
