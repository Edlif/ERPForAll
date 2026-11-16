// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Inventory overview and the transfer views, actions and menus through the real installer.
use super::*;

/// entries open the transfer actions; group visibility is the security tests')
#[test]
fn inventory_opens_on_the_overview_and_operations_open_transfers() {
    run(async {
        let db = TestDb::create().await.unwrap();
        let orm = install(&db).await;
        assert_eq!(
            strings(
                &orm,
                "SELECT d.name || '|' || v.model || '|' || v.type FROM ir_ui_view v \
                 JOIN ir_model_data d ON d.model = 'ir.ui.view' AND d.res_id = v.id \
                 WHERE d.module = 'stock' AND d.name IN ('stock_picking_type_kanban', \
                 'view_picking_list', 'view_picking_form', 'view_picking_search') ORDER BY d.name"
            )
            .await,
            [
                "stock_picking_type_kanban|stock.picking.type|kanban",
                "view_picking_form|stock.picking|form",
                "view_picking_list|stock.picking|list",
                "view_picking_search|stock.picking|search",
            ]
        );
        // the app opens on its first child by sequence
        assert_eq!(
            strings(
                &orm,
                "SELECT dm.name FROM ir_ui_menu m \
                 JOIN ir_model_data dm ON dm.model = 'ir.ui.menu' AND dm.res_id = m.id \
                 JOIN ir_model_data dr ON dr.model = 'ir.ui.menu' AND dr.res_id = m.parent_id \
                 WHERE dr.module = 'stock' AND dr.name = 'menu_stock_root' \
                 ORDER BY m.sequence, m.id LIMIT 1"
            )
            .await,
            ["menu_stock_overview"]
        );
        assert_eq!(
            strings(
                &orm,
                "SELECT dm.name || '|' || dp.name || '|' || da.name || '|' || a.res_model || '|' || a.view_mode \
                 FROM ir_ui_menu m \
                 JOIN ir_model_data dm ON dm.model = 'ir.ui.menu' AND dm.res_id = m.id \
                 JOIN ir_model_data dp ON dp.model = 'ir.ui.menu' AND dp.res_id = m.parent_id \
                 JOIN ir_model_data da ON da.model = 'ir.actions.act_window' \
                  AND m.action = 'ir.actions.act_window,' || da.res_id \
                 JOIN ir_act_window a ON a.id = da.res_id \
                 WHERE dm.module = 'stock' AND dm.name IN ('menu_stock_overview', \
                 'menu_stock_receipts', 'menu_stock_deliveries', 'menu_stock_internal', \
                 'menu_stock_all_transfers') ORDER BY dm.name"
            )
            .await,
            [
                "menu_stock_all_transfers|menu_stock_operations|action_picking_tree_all|stock.picking|list,form",
                "menu_stock_deliveries|menu_stock_operations|action_picking_tree_outgoing|stock.picking|list,form",
                "menu_stock_internal|menu_stock_operations|action_picking_tree_internal|stock.picking|list,form",
                "menu_stock_overview|menu_stock_root|stock_picking_type_action|stock.picking.type|kanban",
                "menu_stock_receipts|menu_stock_operations|action_picking_tree_incoming|stock.picking|list,form",
            ]
        );
        db.cleanup().await.unwrap();
    });
}
