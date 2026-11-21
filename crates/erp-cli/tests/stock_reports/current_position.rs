// SPDX-License-Identifier: MIT OR Apache-2.0
//! Current product positions from real move and quant rows under the installed stock policy.

use super::*;
use erp_addon_stock::position::{PositionMeasure, PositionRequest, PositionScope, stock_position};

#[test]
fn current_position_matches_visible_quant_and_move_sources() {
    big_stack(verify_current_position);
}

struct PositionAction {
    menu: i64,
    action: i64,
    report: i64,
    reporting: i64,
    inventory: i64,
}

async fn stock_id(tx: &mut Transaction, name: &str) -> i64 {
    tx.query(
        "SELECT res_id FROM ir_model_data WHERE module='stock' AND name=$1",
        &[&name],
    )
    .await
    .unwrap()[0]
        .get(0)
}

async fn verify_current_position() {
    let (db, orm) = install().await;
    let mut tx = orm
        .begin_with(SUPERUSER_ID, Context::new().with_company_id(1))
        .await
        .unwrap();
    let company = one(&mut tx, "SELECT min(id)::int8 FROM res_company").await;
    report_access::admin(&mut tx, company).await;
    let clerk = report_access::user(
        &mut tx,
        "position-stock-clerk",
        company,
        &[("stock", "group_stock_user")],
    )
    .await;
    let outsider = report_access::user(&mut tx, "position-outsider", company, &[]).await;
    let currency = one(
        &mut tx,
        &format!("SELECT currency_id::int8 FROM res_company WHERE id={company}"),
    )
    .await;
    let peer = create(
        &mut tx,
        company,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Other stock position company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let item = product(&mut tx, company, "Product with visible stock").await;
    let no_movements = product(&mut tx, company, "Product without movements").await;
    let category = create(
        &mut tx,
        company,
        "product.category",
        RawVals::new().set_cell("name", text("Position category")),
    )
    .await;
    tx.env()
        .records("product.product")
        .unwrap()
        .browse([item])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("categ_id", Cell::Id(category))
                .set_cell("default_code", text("CUR-17")),
        )
        .await
        .unwrap();
    let report = PositionAction {
        menu: stock_id(&mut tx, "menu_current_product_position").await,
        action: stock_id(&mut tx, "action_current_product_position").await,
        report: stock_id(&mut tx, "report_current_product_position").await,
        reporting: stock_id(&mut tx, "menu_stock_reporting").await,
        inventory: stock_id(&mut tx, "menu_stock_root").await,
    };
    let unit = one(
        &mut tx,
        &format!("SELECT t.uom_id FROM product_product p JOIN product_template t ON t.id=p.product_tmpl_id WHERE p.id={item}"),
    )
    .await;
    let case = create(
        &mut tx,
        company,
        "uom.uom",
        RawVals::new()
            .set_cell("name", text("Position case of three"))
            .set_cell("relative_uom_id", Cell::Id(unit))
            .set_cell("relative_factor", Cell::Decimal(Decimal::from(3))),
    )
    .await;
    let root = one(
        &mut tx,
        &format!("SELECT lot_stock_id FROM stock_warehouse WHERE company_id={company} ORDER BY id LIMIT 1"),
    )
    .await;
    let child = create(
        &mut tx,
        company,
        "stock.location",
        RawVals::new()
            .set_cell("name", text("Position shelf"))
            .set_cell("company_id", Cell::Id(company))
            .set_cell("location_id", Cell::Id(root))
            .set_cell("usage", text("internal")),
    )
    .await;
    let other = create(
        &mut tx,
        company,
        "stock.location",
        RawVals::new()
            .set_cell(
                "name",
                text("Position warehouse outside the selected scope"),
            )
            .set_cell("company_id", Cell::Id(company))
            .set_cell("usage", text("internal")),
    )
    .await;
    let peer_root = one(
        &mut tx,
        &format!(
            "SELECT lot_stock_id FROM stock_warehouse WHERE company_id={peer} ORDER BY id LIMIT 1"
        ),
    )
    .await;
    let supplier = one(
        &mut tx,
        "SELECT res_id::int8 FROM ir_model_data WHERE module='stock' AND name='stock_location_suppliers'",
    )
    .await;

    // Keep unrounded source facts to expose the documented display-rounding order.
    tx.execute(
        "UPDATE decimal_precision SET digits=2 WHERE name='Product Unit'",
        &[],
    )
    .await
    .unwrap();
    let quants: Vec<i64> = tx.query(
        "INSERT INTO stock_quant(company_id,product_id,location_id,quantity,reserved_quantity,in_date)
         VALUES ($1,$2,$3,10.005,2.001,now()),($1,$2,$4,5,0.002,now()),
                ($1,$2,$5,25,0,now()),($6,$2,$7,90,0,now()) RETURNING id",
        &[&company, &item, &root, &child, &other, &peer, &peer_root],
    ).await.unwrap().iter().map(|row| row.get(0)).collect();
    let mut moves = Vec::new();
    for (owner, source, dest, forecast, state, uom, amount, factor) in [
        (
            company,
            supplier,
            root,
            None,
            "confirmed",
            case,
            "2.001",
            3_i32,
        ),
        (
            company,
            supplier,
            other,
            Some(child),
            "waiting",
            unit,
            "2.002",
            1,
        ),
        (company, root, supplier, None, "assigned", unit, "3.004", 1),
        (
            company,
            root,
            child,
            None,
            "partially_available",
            unit,
            "8",
            1,
        ),
        (company, supplier, root, None, "draft", unit, "80", 1),
        (company, supplier, root, None, "cancel", unit, "81", 1),
        (company, supplier, root, None, "done", unit, "82", 1),
        (peer, supplier, peer_root, None, "confirmed", unit, "800", 1),
    ] {
        let id = tx.query(
            "INSERT INTO stock_move
                (date,done_at,state,procure_method,quantity,open_line_version,company_id,product_id,
                 uom_id,location_id,location_dest_id,forecasted_location_id,product_uom_qty,
                 product_qty,quantity_product_uom,uom_factor,product_uom_factor,product_unit_digits,is_scrap,picked)
             VALUES ('2026-01-01',CASE WHEN $1='done' THEN '2026-01-01'::timestamp ELSE NULL END,
                     $1,'make_to_stock',0,0,$2,$3,$4,$5,$6,$7,$8::text::numeric,
                     $8::text::numeric*$9::int4,0,$9::int4,1,3,false,false) RETURNING id",
            &[&state,&owner,&item,&uom,&source,&dest,&forecast,&amount,&factor],
        ).await.unwrap()[0].get::<_, i64>(0);
        moves.push(id);
    }
    tx.commit().await.unwrap();
    drop(tx);

    let users = report_access::policy_orm(&db, &orm).await;
    let mut tx = users
        .begin_with(
            erp_sdk::UserId::new(clerk).unwrap(),
            Context::new().with_company_id(company),
        )
        .await
        .unwrap();
    let env = tx.env();
    assert!(!env.is_superuser());
    check_action(&env, &mut tx, &report).await;
    let scope = PositionScope::locations([root], false).unwrap();
    let request =
        PositionRequest::new([item, no_movements, item], Some(&[company]), scope).unwrap();
    let batch = stock_position(&env, &mut tx, &request).await.unwrap();
    let row = batch.position(item).unwrap();
    for (measure, expected) in [
        (PositionMeasure::OnHand, "15.01"),
        (PositionMeasure::FreeToUse, "13.00"),
        (PositionMeasure::Incoming, "8.01"),
        (PositionMeasure::Outgoing, "3.00"),
        (PositionMeasure::Forecasted, "20.02"),
    ] {
        assert_eq!(row.value(measure), expected.parse::<Decimal>().unwrap());
        assert_eq!(
            batch.position(no_movements).unwrap().value(measure),
            Decimal::ZERO
        );
    }

    for (measure, expected_model, expected_ids) in [
        (PositionMeasure::OnHand, "stock.quant", &quants[..2]),
        (PositionMeasure::FreeToUse, "stock.quant", &quants[..2]),
        (PositionMeasure::Incoming, "stock.move", &moves[..2]),
        (PositionMeasure::Outgoing, "stock.move", &moves[2..3]),
    ] {
        let sources = batch.drill_sources(item, measure).unwrap();
        let [source] = sources.as_slice() else {
            panic!("expected one source")
        };
        assert_eq!(source.model(), expected_model);
        let mut ids = env
            .records(source.model())
            .unwrap()
            .search(&mut tx, source.domain())
            .await
            .unwrap()
            .ids();
        ids.sort_unstable();
        assert_eq!(ids, expected_ids);
    }

    let explicit_empty = PositionRequest::new(
        [item],
        Some(&[company]),
        PositionScope::locations([], false).unwrap(),
    )
    .unwrap();
    let empty = stock_position(&env, &mut tx, &explicit_empty)
        .await
        .unwrap();
    assert_eq!(
        empty.position(item).unwrap().value(PositionMeasure::OnHand),
        Decimal::ZERO
    );
    let strict = PositionRequest::new(
        [item],
        Some(&[company]),
        PositionScope::locations([root], true).unwrap(),
    )
    .unwrap();
    let strict = stock_position(&env, &mut tx, &strict).await.unwrap();
    assert_eq!(
        strict
            .position(item)
            .unwrap()
            .value(PositionMeasure::OnHand),
        "10.01".parse::<Decimal>().unwrap()
    );
    assert_eq!(
        strict
            .position(item)
            .unwrap()
            .value(PositionMeasure::Incoming),
        "6.00".parse::<Decimal>().unwrap()
    );

    let forbidden =
        PositionRequest::new([item], Some(&[peer]), PositionScope::DefaultWarehouses).unwrap();
    let denied = stock_position(&env, &mut tx, &forbidden).await.unwrap_err();
    assert_eq!(denied.kind(), ErrorKind::Access);
    assert_eq!(
        denied.message(),
        "The selected stock scope is not accessible."
    );

    check_report(
        &env,
        &mut tx,
        report.report,
        root,
        category,
        no_movements,
        [
            ("stock.quant", &quants[..2]),
            ("stock.quant", &quants[..2]),
            ("stock.move", &moves[..2]),
            ("stock.move", &moves[2..3]),
        ],
    )
    .await;
    let default = PositionRequest::new([item], None, PositionScope::DefaultWarehouses).unwrap();
    let default = stock_position(&env, &mut tx, &default).await.unwrap();
    assert_eq!(
        default
            .position(item)
            .unwrap()
            .value(PositionMeasure::Forecasted),
        "20.02".parse::<Decimal>().unwrap()
    );
    tx.rollback().await.unwrap();
    drop(tx);

    let mut context = Context::new().with_company_id(company);
    context.set("display_default_code", Value::Bool(false));
    let mut export = users
        .begin_with(erp_sdk::UserId::new(clerk).unwrap(), context)
        .await
        .unwrap();
    check_export_search(&mut export, report.report, root, category).await;
    export.rollback().await.unwrap();
    drop(export);

    let mut outside = users
        .begin_with(
            erp_sdk::UserId::new(outsider).unwrap(),
            Context::new().with_company_id(company),
        )
        .await
        .unwrap();
    let outside_env = outside.env();
    let visible =
        erp_addon_base::ir_ui_menu::IrUiMenu::visible_menu_ids(&outside_env, &mut outside, false)
            .await
            .unwrap();
    assert!(!visible.contains(&report.menu));
    let options = erp_addon_reporting::runtime::report_options(
        &outside_env,
        &mut outside,
        report.report,
        &serde_json::Value::Null,
    )
    .await
    .unwrap()["options"]
        .clone();
    let error = erp_addon_reporting::runtime::report_lines(&outside_env, &mut outside, &options)
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    outside.rollback().await.unwrap();
}

async fn check_export_search(tx: &mut Transaction, report: i64, root: i64, category: i64) {
    use erp_addon_reporting::runtime::{report_lines, report_options};
    use serde_json::json;
    use std::io::{Cursor, Read};

    let env = tx.env();
    let normalized = report_options(
        &env,
        tx,
        report,
        &json!({"provider": {"location_ids": [root], "category_id": category}}),
    )
    .await
    .unwrap();
    assert_eq!(normalized["filters"]["server_search"], true);
    let mut options = normalized["options"].clone();
    options["search"] = json!("CUR-17");
    let lines = report_lines(&env, tx, &options).await.unwrap();
    assert_eq!(lines["lines"].as_array().unwrap().len(), 1);
    assert_eq!(lines["lines"][0]["name"], "Product with visible stock");
    let request = json!({"report": report, "options": options, "format": "xlsx"});
    let prepared = erp_addon_reporting::export::prepare(tx, request.to_string().as_bytes())
        .await
        .unwrap();
    let book = prepared.xlsx().unwrap();
    let mut zip = zip::ZipArchive::new(Cursor::new(&book.bytes)).unwrap();
    let mut labels = String::new();
    zip.by_name("xl/sharedStrings.xml")
        .unwrap()
        .read_to_string(&mut labels)
        .unwrap();
    assert!(labels.contains(">Product with visible stock<"), "{labels}");
}

async fn check_action(env: &Env, tx: &mut Transaction, report: &PositionAction) {
    use erp_addon_web::metadata_test_support::{test_load_client_action, test_load_menus};
    let tree =
        serde_json::to_value(test_load_menus(env, tx, 32 * 1024 * 1024).await.unwrap()).unwrap();
    let nodes = tree["nodes"].as_array().unwrap();
    let find = |id| nodes.iter().find(|node| node["id"] == id).unwrap();
    let menu = find(report.menu);
    assert_eq!(menu["action_id"], report.action);
    assert_eq!(menu["action_kind"], "client");
    assert_eq!(menu["parent_id"], report.reporting);
    assert_eq!(menu["label"], "Product Stock Position");
    assert_eq!(find(report.reporting)["parent_id"], report.inventory);
    assert_eq!(find(report.reporting)["label"], "Reporting");
    assert_eq!(find(report.inventory)["label"], "Inventory");
    let action = serde_json::to_value(
        test_load_client_action(env, tx, report.action)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(action["result"]["name"], "Product Stock Position");
    assert_eq!(action["result"]["tag"], "reporting.report");
    assert_eq!(action["result"]["params"]["report"], report.report);
}

async fn check_report(
    env: &Env,
    tx: &mut Transaction,
    report: i64,
    root: i64,
    category: i64,
    other_product: i64,
    expected: [(&str, &[i64]); 4],
) {
    use erp_addon_reporting::line::{LineId, Segment};
    use erp_addon_reporting::runtime::{report_drill, report_lines, report_options};
    use serde_json::json;
    let normalized = report_options(
        env,
        tx,
        report,
        &json!({"provider": {"location_ids": [root], "category_id": category}}),
    )
    .await
    .unwrap();
    assert_eq!(
        normalized["filters"]["provider"],
        "stock.current_product_position"
    );
    let options = normalized["options"].clone();
    let answer = report_lines(env, tx, &options).await.unwrap();
    let lines = answer["lines"].as_array().unwrap();
    let [row] = lines.as_slice() else {
        panic!("expected one product row: {answer}")
    };
    assert_eq!(row["name"], "[CUR-17] Product with visible stock");

    let mut code_search = options.clone();
    code_search["search"] = json!("CUR-17");
    assert_eq!(
        report_lines(env, tx, &code_search).await.unwrap()["lines"],
        answer["lines"]
    );

    let mut display_search = options.clone();
    display_search["search"] = row["name"].clone();
    let values: Vec<_> = row["cells"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cell| cell["value"].as_str().unwrap())
        .collect();
    assert_eq!(
        values,
        [
            "Position category",
            "15.01",
            "13.00",
            "8.01",
            "3.00",
            "20.02"
        ]
    );

    let line = row["id"].as_str().unwrap();
    for (index, (model, expected_ids)) in expected.into_iter().enumerate() {
        let column = index + 1;
        assert_eq!(row["cells"][column]["drillable"], true);
        let drill = report_drill(env, tx, &display_search, line, &json!({"cell": column}))
            .await
            .unwrap();
        assert_eq!(drill["res_model"], model);
        let mut ids = env
            .records(model)
            .unwrap()
            .search(tx, &Domain::from_json(&drill["domain"]).unwrap())
            .await
            .unwrap()
            .ids();
        ids.sort_unstable();
        assert_eq!(ids, expected_ids);
    }
    assert_eq!(row["cells"][5]["drillable"], false);
    let forecast = report_drill(env, tx, &display_search, line, &json!({"cell": 5}))
        .await
        .unwrap_err();
    assert_eq!(forecast.kind(), ErrorKind::Validation);
    assert_eq!(
        forecast.message(),
        "Forecasted stock combines current stock and open movements. Open its On Hand, Incoming or Outgoing source."
    );
    assert_eq!(row["caret"], json!(["open"]));
    let product = report_drill(env, tx, &display_search, line, &json!({"item": "open"}))
        .await
        .unwrap();
    assert_eq!(product["res_model"], "product.product");
    let selected = LineId::parse(line).unwrap();
    let variant = selected
        .segments()
        .last()
        .unwrap()
        .value()
        .parse::<i64>()
        .unwrap();
    assert_eq!(product["res_id"], variant);

    let forged = LineId::root(report).child(Segment::group(
        "product_id",
        "product.product",
        &other_product.to_string(),
    ));
    let denied = report_drill(env, tx, &options, &forged.to_string(), &json!({"cell": 3}))
        .await
        .unwrap_err();
    assert_eq!(denied.kind(), ErrorKind::Validation);
    assert_eq!(
        denied.message(),
        "The selected product is outside the stock position report."
    );

    let mut historical = options;
    historical["date"]["to"] = json!("2001-01-01");
    let denied = report_lines(env, tx, &historical).await.unwrap_err();
    assert_eq!(denied.kind(), ErrorKind::Validation);
    assert!(denied.message().contains("current stock"), "{denied:?}");
}
