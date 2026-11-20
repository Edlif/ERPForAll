// SPDX-License-Identifier: MIT OR Apache-2.0
//! Persisted barcode resolution through the real Settings and print services.
use super::*;
use erp_addon_base::ir_config_setting::{describe, read_values, write_values};
use erp_sdk::views::wire::{SettingValue, SettingsScope, ValueSource};
use erp_sdk::{ErrorKind, Value};

const KEY: &str = "base.report_barcode_dpi";
const RANGE: &str = "base.report_barcode_dpi takes a whole number from 72 to 2400.";

#[test]
fn barcode_dpi_is_a_persisted_bounded_setting() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let orm = system(&db)
            .initialize(InitOptions::new())
            .await
            .unwrap()
            .orm;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let env = tx.env();
        let description = describe(&env, &mut tx, SettingsScope::Global {})
            .await
            .unwrap();
        let declaration = description
            .declarations
            .iter()
            .find(|item| item.key == KEY)
            .expect("base must publish its barcode printer resolution setting");
        assert_eq!(declaration.block, "Reports");
        assert!(declaration.writable);
        assert!(matches!(declaration.default, SettingValue::Integer(300)));
        let initial = read_values(&env, &mut tx, SettingsScope::Global {}, vec![KEY.into()])
            .await
            .unwrap();
        assert!(matches!(
            initial.values[0].value,
            SettingValue::Integer(300)
        ));
        assert!(matches!(initial.values[0].source, ValueSource::Default));
        let mut revision = initial.revision;
        for dpi in [72, 2400, 203] {
            let saved = write_values(
                &env,
                &mut tx,
                SettingsScope::Global {},
                revision,
                vec![(KEY.into(), Value::Int(dpi))],
            )
            .await
            .unwrap();
            assert!(matches!(saved.values[0].value, SettingValue::Integer(value) if value == dpi));
            revision = saved.revision;
        }
        for value in [
            Value::Int(0),
            Value::Int(-1),
            Value::Int(71),
            Value::Int(2401),
            Value::Int(i64::MAX),
            Value::Float(203.5),
            Value::str("203"),
        ] {
            let error = write_values(
                &env,
                &mut tx,
                SettingsScope::Global {},
                revision.clone(),
                vec![(KEY.into(), value)],
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert_eq!(error.message(), RANGE);
            let unchanged = read_values(&env, &mut tx, SettingsScope::Global {}, vec![KEY.into()])
                .await
                .unwrap();
            assert_eq!(unchanged.revision, revision);
            assert!(matches!(
                unchanged.values[0].value,
                SettingValue::Integer(203)
            ));
        }
        let error = write_values(
            &env,
            &mut tx,
            SettingsScope::Global {},
            "0".into(),
            vec![(KEY.into(), Value::Int(300))],
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Conflict);
        assert_eq!(error.message(), "Settings changed; reload before saving.");
        tx.commit().await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let saved = read_values(
            &tx.env(),
            &mut tx,
            SettingsScope::Global {},
            vec![KEY.into()],
        )
        .await
        .unwrap();
        assert!(matches!(saved.values[0].value, SettingValue::Integer(203)));
        assert!(matches!(saved.values[0].source, ValueSource::Stored));
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

async fn fixture_report(
    tx: &mut erp_sdk::Transaction,
) -> (erp_addon_base::report_print::PrintReport, Vec<i64>) {
    let key = "test_dpi.report_partner";
    let arch = r#"<t t-name="test_dpi.report_partner"><t t-foreach="docs" t-as="doc"><div><span t-field="doc.name" t-options="{'widget': 'barcode', 'img_style': 'width:300px;height:50px'}"/><span t-field="doc.name" t-options="{'widget': 'barcode', 'dpi': 300, 'img_style': 'width:300px;height:50px'}"/></div></t></t>"#;
    let env = tx.env();
    env.records("ir.ui.view")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str(key.into()))
                    .set_cell("type", Cell::Str("qweb".into()))
                    .set_cell("key", Cell::Str(key.into()))
                    .set_cell("arch_db", Cell::Str(arch.into())),
            ],
        )
        .await
        .unwrap();
    env.records("ir.actions.report")
        .unwrap()
        .create(
            tx,
            vec![
                RawVals::new()
                    .set_cell("name", Cell::Str("Barcode DPI sheet".into()))
                    .set_cell("model", Cell::Str("res.partner".into()))
                    .set_cell("report_type", Cell::Str("qweb-pdf".into()))
                    .set_cell("report_name", Cell::Str(key.into())),
            ],
        )
        .await
        .unwrap();
    let ids = env
        .records("res.partner")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str("WH/OUT/00012".into()))],
        )
        .await
        .unwrap()
        .ids();
    let report = erp_addon_base::report_print::find(tx, key).await.unwrap();
    (report, ids)
}

fn modules(doc: &erp_report::DocTree) -> Vec<f64> {
    report_frame::through_frame(doc)
        .nodes()
        .iter()
        .filter_map(|node| match node {
            erp_report::Node::Vector { vector, width, .. } => Some(width.unwrap() / vector.width()),
            _ => None,
        })
        .collect()
}

#[test]
fn barcode_dpi_preparation_uses_settings_and_refuses_corruption() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let orm = system(&db)
            .initialize(InitOptions::new())
            .await
            .unwrap()
            .orm;
        let mut tx = orm
            .begin_with(SUPERUSER_ID, erp_orm::Context::new().with_company_id(1))
            .await
            .unwrap();
        let (report, ids) = fixture_report(&mut tx).await;
        let initial = modules(
            &erp_addon_base::report_print::prepare(&mut tx, &report, &ids)
                .await
                .unwrap(),
        );
        assert_eq!(initial.len(), 2);
        assert_eq!(initial[0], initial[1]);
        // This fixture distinguishes 300-DPI output from 203-DPI output.
        let old_dots = initial[0] * 203.0 / 72.0;
        assert!((old_dots - old_dots.round()).abs() > 1e-6);
        let current = read_values(
            &tx.env(),
            &mut tx,
            SettingsScope::Global {},
            vec![KEY.into()],
        )
        .await
        .unwrap();
        write_values(
            &tx.env(),
            &mut tx,
            SettingsScope::Global {},
            current.revision,
            vec![(KEY.into(), Value::Int(203))],
        )
        .await
        .unwrap();
        let currency: i64 = tx
            .query("SELECT currency_id::int8 FROM res_company WHERE id=1", &[])
            .await
            .unwrap()[0]
            .get(0);
        let other_company = tx
            .env()
            .records("res.company")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Second DPI company".into()))
                        .set_cell("currency_id", Cell::Id(currency)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let other_partner = tx
            .env()
            .records("res.partner")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("WH/OUT/00012".into()))
                        .set_cell("company_id", Cell::Id(other_company)),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let both = [ids[0], other_partner];
        let documents = erp_addon_base::report_print::prepare_runs(&mut tx, &report, &both)
            .await
            .unwrap();
        assert_eq!(documents.len(), 2, "one document per company run");
        for document in documents {
            let module = modules(&document);
            assert_eq!(module.len(), 2);
            let dots = module[0] * 203.0 / 72.0;
            assert!(dots > 0.0 && (dots - dots.round()).abs() < 1e-9);
            assert_ne!(module[0], initial[0]);
            assert_eq!(module[1], initial[1], "explicit template DPI wins");
        }
        for corrupt in ["0", "2401", "\"203\"", "203.5", "null", "broken"] {
            tx.execute(
                "UPDATE ir_config_setting_value SET value_text=$1 WHERE setting_key=$2",
                &[&corrupt, &KEY],
            )
            .await
            .unwrap();
            let error = erp_addon_base::report_print::prepare(&mut tx, &report, &ids)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation);
            assert_eq!(error.message(), format!("Stored setting {KEY} is invalid."));
        }
        tx.execute(
            "UPDATE ir_config_setting_value SET value_text='300' WHERE setting_key=$1",
            &[&KEY],
        )
        .await
        .unwrap();
        assert_eq!(
            modules(
                &erp_addon_base::report_print::prepare(&mut tx, &report, &ids)
                    .await
                    .unwrap()
            ),
            initial
        );
        tx.batch("ALTER TABLE ir_config_setting_value RENAME TO unavailable_dpi_settings")
            .await
            .unwrap();
        let error = erp_addon_base::report_print::prepare(&mut tx, &report, &ids)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Database);
        assert_eq!(error.sqlstate(), Some("42P01"));
        // Driver context is version-specific; SQLSTATE identifies the missing table.
        assert!(!error.message().is_empty());
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}
