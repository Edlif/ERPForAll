//! Company preferences (owner request 2026-11-05): the
//! company's contact fields mirror its partner, a company without a partner gets one, the
//! Settings > Companies screens carry the fields, and only administrators change a company.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration assertions"
)]
use std::{path::Path, sync::Arc};

use erp_fields::{Cell, ErrorKind, RecId};
use erp_modules::{AddonIndex, InitOptions, ModuleName, ModuleSystem};
use erp_orm::{Command, Orm, RawVals, SUPERUSER_ID, Transaction};
use erp_security::{resolver::Resolver, source::PolicySource};
use erp_testkit::{Harness, TestDb};

const MAX_LABEL_BYTES: usize = 1_048_576;

fn text(value: &str) -> Cell {
    Cell::Str(value.into())
}

async fn create(tx: &mut Transaction, model: &str, vals: RawVals) -> i64 {
    tx.env()
        .records(model)
        .unwrap()
        .create(tx, vec![vals])
        .await
        .unwrap()
        .ids()[0]
}

async fn cell(tx: &mut Transaction, model: &str, id: i64, field: &str) -> Cell {
    tx.env()
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
        .cell(tx, field)
        .await
        .unwrap()
}

async fn currency(tx: &mut Transaction) -> i64 {
    create(
        tx,
        "res.currency",
        RawVals::new()
            .set_cell("name", text("XCP"))
            .set_cell("symbol", text("X")),
    )
    .await
}

async fn partner_count(tx: &mut Transaction) -> u64 {
    tx.env()
        .records("res.partner")
        .unwrap()
        .search_count(tx, &erp_sdk::domain::Domain::constant(true), None)
        .await
        .unwrap()
}

/// second one; case (a), the missing-name refusal, and the rename of the given partner to the
/// company name are not asserted here, the rename is not implemented)
#[tokio::test]
async fn create_without_partner_creates_one_and_keeps_a_given_partner() {
    let harness = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = harness.begin().await.unwrap();
    let currency = currency(&mut tx).await;
    // The partner's own website normalisation is the reference for the company route.
    let reference = create(
        &mut tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Website Reference"))
            .set_cell("website", text("acme.example")),
    )
    .await;
    let normalised = cell(&mut tx, "res.partner", reference, "website").await;
    assert_ne!(
        normalised,
        text("acme.example"),
        "the partner normalises a bare host"
    );
    let parent_company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Parent Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let before = partner_count(&mut tx).await;
    let companies = tx
        .env()
        .records("res.company")
        .unwrap()
        .with_context([("default_parent_id", erp_fields::Value::Int(parent_company))]);
    let company = companies
        .create(
            &mut tx,
            vec![
                RawVals::new()
                    .set_cell("name", text("Acme Holdings"))
                    .set_cell("currency_id", Cell::Id(currency))
                    .set_cell("website", text("acme.example"))
                    .set_cell("street", text("1 Main Road"))
                    .set_cell("city", text("Jakarta"))
                    .set_cell("zip", text("10110"))
                    .set_cell("email", text("info@acme.example"))
                    .set_cell("phone", text("+62 21 555 0100"))
                    .set_cell("vat", text("ID-123")),
            ],
        )
        .await
        .unwrap()
        .ids()[0];
    assert_eq!(partner_count(&mut tx).await, before + 1);
    let Cell::Id(partner) = cell(&mut tx, "res.company", company, "partner_id").await else {
        panic!("a company created with a name must get a partner");
    };
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "name").await,
        text("Acme Holdings")
    );
    assert_eq!(
        cell(&mut tx, "res.company", company, "parent_id").await,
        Cell::Id(parent_company),
        "the company itself takes the default parent"
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "parent_id").await,
        Cell::Null,
        "the default parent from the context is dropped for the partner"
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "company_id").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "website").await,
        normalised
    );
    // Read in the same transaction, the company keeps the value as written; the partner holds the
    // normalised value read above.
    assert_eq!(
        cell(&mut tx, "res.company", company, "website").await,
        text("acme.example")
    );
    for (field, value) in [
        ("street", "1 Main Road"),
        ("city", "Jakarta"),
        ("zip", "10110"),
        ("email", "info@acme.example"),
        ("phone", "+62 21 555 0100"),
        ("vat", "ID-123"),
    ] {
        assert_eq!(
            cell(&mut tx, "res.partner", partner, field).await,
            text(value),
            "partner {field}"
        );
        assert_eq!(
            cell(&mut tx, "res.company", company, field).await,
            text(value),
            "company {field}"
        );
    }
    // A company that names its partner does not get another one.
    let existing = create(
        &mut tx,
        "res.partner",
        RawVals::new().set_cell("name", text("Existing Partner")),
    )
    .await;
    let counted = partner_count(&mut tx).await;
    let second = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Second Company"))
            .set_cell("currency_id", Cell::Id(currency))
            .set_cell("partner_id", Cell::Id(existing)),
    )
    .await;
    assert_eq!(partner_count(&mut tx).await, counted);
    assert_eq!(
        cell(&mut tx, "res.company", second, "partner_id").await,
        Cell::Id(existing)
    );
    tx.rollback().await.unwrap();
    harness.cleanup().await.unwrap();
}

/// partner's own address)
#[tokio::test]
async fn company_and_partner_edits_stay_in_sync_both_ways() {
    let harness = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = harness.begin().await.unwrap();
    let currency = currency(&mut tx).await;
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Sync Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let Cell::Id(partner) = cell(&mut tx, "res.company", company, "partner_id").await else {
        panic!("a company created with a name must get a partner");
    };
    // company -> partner
    tx.env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("street", text("2 Side Street"))
                .set_cell("street2", text("Floor 3"))
                .set_cell("website", text("sync.example"))
                .set_cell("email", text("hello@sync.example"))
                .set_cell("vat", text("ID-SYNC"))
                .set_cell("phone", text("+62 22 000"))
                .set_cell("city", text("Surabaya"))
                .set_cell("zip", text("60111")),
        )
        .await
        .unwrap();
    // The partner's own normalisation of a bare host is the reference for the company route.
    let reference = create(
        &mut tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text("Website Reference"))
            .set_cell("website", text("sync.example")),
    )
    .await;
    let normalised = cell(&mut tx, "res.partner", reference, "website").await;
    assert_ne!(normalised, text("sync.example"));
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "website").await,
        normalised,
        "partner website"
    );
    // Same transaction: the company keeps the value as written (see the create test above); the
    // partner holds the normalised value.
    assert_eq!(
        cell(&mut tx, "res.company", company, "website").await,
        text("sync.example"),
        "company website read after the write, same transaction"
    );
    for (field, value) in [
        ("vat", "ID-SYNC"),
        ("phone", "+62 22 000"),
        ("city", "Surabaya"),
        ("zip", "60111"),
    ] {
        assert_eq!(
            cell(&mut tx, "res.partner", partner, field).await,
            text(value),
            "partner {field}"
        );
    }
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "street").await,
        text("2 Side Street")
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "street2").await,
        text("Floor 3")
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "email").await,
        text("hello@sync.example")
    );
    // partner -> company
    tx.env()
        .records("res.partner")
        .unwrap()
        .browse([partner])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new()
                .set_cell("phone", text("+62 811 000"))
                .set_cell("city", text("Bandung"))
                .set_cell("zip", text("40111")),
        )
        .await
        .unwrap();
    for (field, value) in [
        ("phone", "+62 811 000"),
        ("city", "Bandung"),
        ("zip", "40111"),
        ("street", "2 Side Street"),
    ] {
        assert_eq!(
            cell(&mut tx, "res.company", company, field).await,
            text(value),
            "company {field}"
        );
    }
    // Both survive a commit and a fresh transaction.
    tx.commit().await.unwrap();
    let mut read = harness.begin().await.unwrap();
    assert_eq!(
        cell(&mut read, "res.company", company, "website").await,
        normalised,
        "company website read in fresh transaction reflects partner normalisation (S-r3-1)"
    );
    assert_eq!(
        cell(&mut read, "res.company", company, "city").await,
        text("Bandung")
    );
    assert_eq!(
        cell(&mut read, "res.partner", partner, "street").await,
        text("2 Side Street")
    );
    read.rollback().await.unwrap();
    harness.cleanup().await.unwrap();
}

#[tokio::test]
async fn partner_input_bounds_apply_to_company_edits() {
    let harness = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = harness.begin().await.unwrap();
    let currency = currency(&mut tx).await;
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Bounded Company"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    let companies = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    let exact = "a".repeat(MAX_LABEL_BYTES);
    companies
        .write(&mut tx, RawVals::new().set_cell("street", text(&exact)))
        .await
        .unwrap();
    let Cell::Id(partner) = cell(&mut tx, "res.company", company, "partner_id").await else {
        panic!("a company created with a name must get a partner");
    };
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "street").await,
        text(&exact),
        "the exact-size street is accepted and stored on the partner"
    );
    assert_eq!(
        cell(&mut tx, "res.company", company, "street").await,
        text(&exact)
    );
    let over = "a".repeat(MAX_LABEL_BYTES + 1);
    let error = tx
        .savepoint(async |tx| {
            companies
                .write(tx, RawVals::new().set_cell("city", text(&over)))
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert!(
        error.message().contains("res.partner.city exceeds"),
        "{}",
        error.message()
    );
    // Nothing changed: the refused city is not stored, the accepted street is still there.
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "city").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.company", company, "city").await,
        Cell::Null
    );
    // Email input is bounded at 64 KiB by the partner's own admission.
    let long_email = format!("{}@example.invalid", "a".repeat(65_536));
    let error = tx
        .savepoint(async |tx| {
            companies
                .write(tx, RawVals::new().set_cell("email", text(&long_email)))
                .await
        })
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{error:?}");
    assert!(
        error
            .message()
            .contains("Contact email input exceeds the 65536-byte limit"),
        "the message must name the email field: {}",
        error.message()
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "email").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.company", company, "email").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "street").await,
        text(&exact)
    );
    tx.rollback().await.unwrap();
    harness.cleanup().await.unwrap();
}

fn user_command(ids: &[i64]) -> Command {
    Command::set(ids.iter().map(|id| RecId::from_real(*id).unwrap()))
}

async fn user(tx: &mut Transaction, name: &str, company: i64, groups: &[i64]) -> i64 {
    let partner = create(
        tx,
        "res.partner",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("company_id", Cell::Id(company)),
    )
    .await;
    create(
        tx,
        "res.users",
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("login", text(name))
            .set_cell("partner_id", Cell::Id(partner))
            .set_cell("company_id", Cell::Id(company))
            .set_cell("lang", text("en_US"))
            .set_cell("tz", text("UTC"))
            .set_commands("company_ids", [user_command(&[company])])
            .set_commands("group_ids", [user_command(groups)]),
    )
    .await
}

async fn xid(tx: &mut Transaction, module: &str, name: &str, model: &str) -> i64 {
    tx.query(
        "SELECT res_id FROM ir_model_data WHERE module=$1 AND name=$2 AND model=$3",
        &[&module, &name, &model],
    )
    .await
    .unwrap()[0]
        .get(0)
}

#[test]
fn installed_stack_screens_access_and_defaults() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(installed());
        })
        .unwrap()
        .join()
        .unwrap();
}

#[allow(clippy::too_many_lines, reason = "one installed-stack walk")]
async fn installed() {
    let db = TestDb::create().await.unwrap();
    let mut index = AddonIndex::new(true);
    index.add_dev_path(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("addons"),
    );
    let identities = index
        .discover_entries()
        .unwrap()
        .into_iter()
        .map(|(name, entry)| {
            let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(
                name.as_str(),
                Path::new(entry.source_location()),
            )
            .unwrap();
            (name, snapshot.digest().to_owned())
        })
        .collect();
    let system = ModuleSystem::builder(db.pool().clone(), index)
        .native_artifact_identities(identities)
        .code([
            &erp_addon_base::ADDON,
            &erp_addon_web::ADDON,
            &erp_addon_uom::ADDON,
        ])
        .view_types(erp_addon_web::view_type_contributions())
        .require_core_tables(erp_security::auth::core_tables())
        .build()
        .unwrap();
    let loaded = system.initialize(InitOptions::new()).await.unwrap();
    assert!(loaded.post_commit.is_clean());
    let mut tx = loaded.orm.begin(SUPERUSER_ID).await.unwrap();
    let internal = xid(&mut tx, "base", "group_user", "res.groups").await;
    let system_group = xid(&mut tx, "base", "group_system", "res.groups").await;
    let company = xid(&mut tx, "base", "main_company", "res.company").await;

    // A second company whose partner carries that company (allows exactly this
    // value), for the cross-company reads and 012.
    let main_currency = cell(&mut tx, "res.company", company, "currency_id").await;
    let other = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Other Company"))
            .set_cell("currency_id", main_currency)
            .set_cell("email", text("other@company.example"))
            .set_cell("street", text("5 Far Road"))
            .set_cell("vat", text("ID-OTHER")),
    )
    .await;
    let Cell::Id(other_partner) = cell(&mut tx, "res.company", other, "partner_id").await else {
        panic!("a company created with a name must get a partner");
    };
    tx.env()
        .records("res.partner")
        .unwrap()
        .browse([other_partner])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("company_id", Cell::Id(other)),
        )
        .await
        .unwrap();

    let main_partner = xid(&mut tx, "base", "main_partner", "res.partner").await;
    assert_eq!(
        cell(&mut tx, "res.company", company, "partner_id").await,
        Cell::Id(main_partner)
    );

    let list = xid(&mut tx, "base", "settings_companies_list", "ir.ui.view").await;
    let form = xid(&mut tx, "base", "settings_companies_form", "ir.ui.view").await;
    let env = tx.env().clone();
    let described = erp_addon_web::metadata_test_support::test_get_views(
        &env,
        &mut tx,
        "res.company",
        vec![
            erp_fields::Value::list([erp_fields::Value::Int(list), erp_fields::Value::str("list")]),
            erp_fields::Value::list([erp_fields::Value::Int(form), erp_fields::Value::str("form")]),
        ],
        32 * 1024 * 1024,
    )
    .await
    .unwrap();
    let described = serde_json::to_value(described).unwrap();
    assert!(described["notices"].as_array().unwrap().is_empty());
    let fields = described["fields"]["res.company"].as_object().unwrap();
    let views = described["views"].as_array().unwrap();
    assert_eq!(views.len(), 2);
    let form_json = views[1].to_string();
    for field in [
        "name",
        "street",
        "street2",
        "city",
        "zip",
        "phone",
        "email",
        "website",
        "vat",
        "currency_id",
        "tz",
        "parent_id",
    ] {
        assert!(fields.contains_key(field), "field catalogue lacks {field}");
        assert!(
            form_json.contains(&format!("\"{field}\"")),
            "form view lacks {field}"
        );
    }
    for token in [
        "\"action_all_company_branches\"",
        "\"child_ids\"",
        "\"all_child_ids\"",
        "\"Branches\"",
        "\"General Information\"",
    ] {
        assert!(form_json.contains(token), "form view lacks {token}");
    }
    let list_json = views[0].to_string();
    for field in [
        "logo_web",
        "name",
        "parent_id",
        "email",
        "phone",
        "currency_id",
    ] {
        assert!(
            list_json.contains(&format!("\"{field}\"")),
            "list view lacks {field}"
        );
    }
    let title_children =
        title_block_children(&views[1]).expect("the company form has a title block");
    assert!(
        title_children
            .iter()
            .any(|child| child.contains("\"logo\"") && child.contains("\"image\"")),
        "the title block lacks the logo picture: {title_children:?}"
    );
    assert!(
        title_children
            .iter()
            .any(|child| child.contains("\"heading\"")),
        "the title block lacks the name heading: {title_children:?}"
    );

    // Subjects: a bootstrap administrator keeps the last-administrator invariant, then one
    // administrator and one ordinary internal user.
    user(
        &mut tx,
        "company-bootstrap",
        company,
        &[internal, system_group],
    )
    .await;
    let admin = user(&mut tx, "company-admin", company, &[internal, system_group]).await;
    let ordinary = user(&mut tx, "company-ordinary", company, &[internal]).await;
    tx.env()
        .records("res.partner")
        .unwrap()
        .browse([main_partner])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("email", text("main@company.example")),
        )
        .await
        .unwrap();
    let main_partner_before = [
        cell(&mut tx, "res.partner", main_partner, "name").await,
        cell(&mut tx, "res.partner", main_partner, "street").await,
        cell(&mut tx, "res.partner", main_partner, "email").await,
    ];
    let main_company_name_before = cell(&mut tx, "res.company", company, "name").await;
    tx.commit().await.unwrap();
    drop(tx);

    let source = PolicySource::build(Arc::clone(loaded.orm.registry()), db.pool().clone())
        .await
        .unwrap();
    let table = loaded
        .orm
        .env(SUPERUSER_ID, erp_orm::Context::new())
        .generation_attachment::<erp_sdk::views::PublishedViewTypes>()
        .unwrap();
    let provider: Arc<dyn erp_orm::PolicyProvider> = Arc::new(source);
    let orm = Orm::builder(Arc::clone(loaded.orm.registry()), db.pool().clone())
        .require_core_tables(erp_security::auth::core_tables())
        .resolver(Resolver::for_registry(loaded.orm.registry()).unwrap())
        .policy(provider)
        .generation_attachment(erp_orm::GenerationAttachment::new(
            Arc::clone(loaded.orm.registry()),
            table,
        ))
        .impls(erp_modules::base_models::IMPLS.iter().copied())
        .constraints(erp_modules::base_models::CONSTRAINTS.iter().copied())
        .unlink_hook(erp_data::model_data::UNLINK_HOOK)
        .build()
        .unwrap();

    // company, and the mirrored value is the partner's.
    let mut ordinary_tx = orm
        .begin(erp_orm::UserId::new(ordinary).unwrap())
        .await
        .unwrap();
    assert_eq!(
        cell(&mut ordinary_tx, "res.company", company, "email").await,
        text("main@company.example")
    );

    let companies = ordinary_tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    for field in ["street", "name", "email"] {
        let error = ordinary_tx
            .savepoint(async |tx| {
                companies
                    .write(tx, RawVals::new().set_cell(field, text("x")))
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{field}: {error:?}");
    }
    // Nothing was written: the company name and the main partner (readable by internal users)
    // are as before.
    assert_eq!(
        cell(&mut ordinary_tx, "res.company", company, "name").await,
        main_company_name_before
    );
    for (field, before) in ["name", "street", "email"].iter().zip(&main_partner_before) {
        assert_eq!(
            &cell(&mut ordinary_tx, "res.partner", main_partner, field).await,
            before,
            "main partner {field} unchanged after the refused writes"
        );
    }

    for field in ["email", "street", "vat"] {
        let error = ordinary_tx
            .savepoint(async |tx| {
                tx.env()
                    .records("res.company")
                    .unwrap()
                    .browse([other])
                    .unwrap()
                    .cell(tx, field)
                    .await
            })
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{field}: {error:?}");
    }
    let ordinary_env = ordinary_tx.env();
    let allowed_day = erp_addon_base::company_calendar::business_days(
        &mut ordinary_tx,
        &ordinary_env,
        &[company],
    )
    .await
    .unwrap();
    assert!(allowed_day.contains_key(&company));
    let denied_records = ordinary_env
        .records("res.company")
        .unwrap()
        .browse([company, other])
        .unwrap();
    let policy_error = denied_records
        .check_access(&mut ordinary_tx, erp_sdk::Operation::Read)
        .await
        .unwrap_err();
    let calendar_error = erp_addon_base::company_calendar::business_days(
        &mut ordinary_tx,
        &ordinary_env,
        &[company, other],
    )
    .await
    .unwrap_err();
    assert_eq!(calendar_error.kind(), ErrorKind::Access);
    // Policy diagnostics include installed record/rule identities; require their full propagation.
    assert_eq!(calendar_error.message(), policy_error.message());
    ordinary_tx.rollback().await.unwrap();
    drop(ordinary_tx);

    let mut admin_tx = orm
        .begin(erp_orm::UserId::new(admin).unwrap())
        .await
        .unwrap();
    for (field, value) in [
        ("email", "other@company.example"),
        ("street", "5 Far Road"),
        ("vat", "ID-OTHER"),
    ] {
        assert_eq!(
            cell(&mut admin_tx, "res.company", other, field).await,
            text(value),
            "administrator reads {field} of the other company"
        );
    }

    // partner follows.
    admin_tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(
            &mut admin_tx,
            RawVals::new()
                .set_cell("street", text("9 Admin Avenue"))
                .set_cell("phone", text("+62 21 999")),
        )
        .await
        .unwrap();
    assert_eq!(
        cell(&mut admin_tx, "res.partner", main_partner, "street").await,
        text("9 Admin Avenue")
    );
    assert_eq!(
        cell(&mut admin_tx, "res.partner", main_partner, "phone").await,
        text("+62 21 999")
    );
    admin_tx.rollback().await.unwrap();
    drop(admin_tx);
    company_zone_survives_base_upgrade(&db, &system, &loaded.orm, company).await;
}

async fn company_zone_survives_base_upgrade(
    db: &TestDb,
    system: &ModuleSystem,
    orm: &Orm,
    company: i64,
) {
    let client = db.client().await.unwrap();
    client
        .batch_execute(
            "BEGIN;
         DELETE FROM ir_model_data WHERE model='ir.model.fields' AND res_id IN
            (SELECT id FROM ir_model_fields WHERE model='res.company' AND name='tz');
         DELETE FROM ir_model_fields WHERE model='res.company' AND name='tz';
         ALTER TABLE res_company DROP COLUMN tz;
         COMMIT;",
        )
        .await
        .unwrap();
    drop(client);
    let mut current = orm.clone();
    for (expected, next) in [("UTC", Some("Asia/Jakarta")), ("Asia/Jakarta", None)] {
        let env = current.env(SUPERUSER_ID, erp_sdk::Context::new());
        let updated =
            Box::pin(system.button_immediate_upgrade(&env, &[ModuleName::new("base").unwrap()]))
                .await
                .unwrap();
        assert!(updated.post_commit.is_clean());
        let mut tx = updated.orm.begin(SUPERUSER_ID).await.unwrap();
        let row = tx
            .env()
            .records("res.company")
            .unwrap()
            .browse([company])
            .unwrap();
        assert_eq!(row.cell(&mut tx, "tz").await.unwrap(), text(expected));
        if let Some(zone) = next {
            row.write(&mut tx, RawVals::new().set_cell("tz", text(zone)))
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        current = updated.orm;
    }
}

/// The children of the first title block of a described view, each as its JSON text: the node
/// arena is the array that holds the `title` node, and its `children` index that array.
fn title_block_children(view: &serde_json::Value) -> Option<Vec<String>> {
    match view {
        serde_json::Value::Array(items) => {
            if let Some(title) = items.iter().find(|item| item["kind"] == "title") {
                let children = title["children"].as_array()?;
                return children
                    .iter()
                    .map(|index| {
                        let index = usize::try_from(index.as_u64()?).ok()?;
                        items.get(index).map(ToString::to_string)
                    })
                    .collect();
            }
            items.iter().find_map(title_block_children)
        }
        serde_json::Value::Object(members) => members.values().find_map(title_block_children),
        _ => None,
    }
}

#[tokio::test]
async fn company_business_zone_is_required_and_invalid_changes_leave_it_intact() {
    let harness = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = harness.begin().await.unwrap();
    let currency = currency(&mut tx).await;
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Jakarta warehouse"))
            .set_cell("currency_id", Cell::Id(currency))
            .set_cell("tz", text("Asia/Jakarta")),
    )
    .await;
    let record = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    assert_eq!(
        record.cell(&mut tx, "tz").await.unwrap(),
        text("Asia/Jakarta")
    );
    for (invalid, message) in [
        (
            text("Warehouse/West"),
            "Unknown company time zone 'Warehouse/West'.",
        ),
        (text(""), "Unknown company time zone ''."),
        (Cell::Null, "Choose a company time zone."),
    ] {
        let error = record
            .write(&mut tx, RawVals::new().set_cell("tz", invalid))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation);
        assert_eq!(error.message(), message);
        assert_eq!(
            record.cell(&mut tx, "tz").await.unwrap(),
            text("Asia/Jakarta")
        );
    }
}

#[tokio::test]
async fn companies_use_their_own_day_at_one_transaction_start() {
    let harness = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = harness.begin().await.unwrap();
    let env = tx.env();
    let currency = currency(&mut tx).await;
    env.records("res.users")
        .unwrap()
        .browse([env.uid()])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("tz", text("Pacific/Kiritimati")),
        )
        .await
        .unwrap();
    let vals = |name| {
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("currency_id", Cell::Id(currency))
    };
    let form_defaults: erp_sdk::ValueMap = env
        .records("res.company")
        .unwrap()
        .call(&mut tx, "default_get", (vec!["tz".to_owned()],))
        .await
        .unwrap();
    assert_eq!(
        form_defaults.get("tz"),
        Some(&erp_fields::Value::Str("Pacific/Kiritimati".into()))
    );
    let company = create(&mut tx, "res.company", vals("East")).await;
    let west = create(
        &mut tx,
        "res.company",
        vals("West").set_cell("tz", text("Pacific/Pago_Pago")),
    )
    .await;
    assert_eq!(
        cell(&mut tx, "res.company", company, "tz").await,
        text("Pacific/Kiritimati")
    );
    let days = erp_addon_base::company_calendar::business_days(&mut tx, &env, &[company, west])
        .await
        .unwrap();
    let rows = tx
        .query(
            "SELECT (transaction_timestamp() AT TIME ZONE 'Pacific/Kiritimati')::date,
                (transaction_timestamp() AT TIME ZONE 'Pacific/Pago_Pago')::date",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(days[&company].date, rows[0].get::<_, erp_fields::Date>(0));
    assert_eq!(days[&west].date, rows[0].get::<_, erp_fields::Date>(1));
    assert!(days[&company].date > days[&west].date);
}

#[tokio::test]
async fn company_calendar_defaults_and_transaction_clock_are_explicit() {
    let h = Harness::builder([&erp_addon_base::ADDON])
        .build()
        .await
        .unwrap();
    let mut tx = h.begin().await.unwrap();
    let env = tx.env();
    let currency = currency(&mut tx).await;
    let vals = |name| {
        RawVals::new()
            .set_cell("name", text(name))
            .set_cell("currency_id", Cell::Id(currency))
    };
    let company = create(&mut tx, "res.company", vals("UTC fallback")).await;
    assert_eq!(
        cell(&mut tx, "res.company", company, "tz").await,
        text("UTC")
    );
    let model = env
        .with_context([("default_tz", erp_fields::Value::str("Asia/Jakarta"))])
        .records("res.company")
        .unwrap();
    let rows = model
        .create(
            &mut tx,
            vec![
                vals("Context"),
                vals("Explicit").set_cell("tz", text("Pacific/Pago_Pago")),
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        rows.cells(&mut tx, "tz").await.unwrap(),
        vec![text("Asia/Jakarta"), text("Pacific/Pago_Pago")]
    );
    let count = env
        .records("res.company")
        .unwrap()
        .search_count(&mut tx, &erp_sdk::domain::Domain::constant(true), None)
        .await
        .unwrap();
    let error = model
        .create(
            &mut tx,
            vec![
                vals("Rollback"),
                vals("Bad").set_cell("tz", text("No/SuchZone")),
            ],
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(error.message(), "Unknown company time zone 'No/SuchZone'.");
    assert_eq!(
        env.records("res.company")
            .unwrap()
            .search_count(&mut tx, &erp_sdk::domain::Domain::constant(true), None)
            .await
            .unwrap(),
        count
    );
    let first = erp_addon_base::company_calendar::business_days(&mut tx, &env, &[company, company])
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    tx.query("SET LOCAL TIME ZONE 'Pacific/Kiritimati'", &[])
        .await
        .unwrap();
    let other_env = env.with_context([("tz", erp_fields::Value::str("Pacific/Pago_Pago"))]);
    let second = erp_addon_base::company_calendar::business_days(&mut tx, &other_env, &[company])
        .await
        .unwrap();
    assert_eq!(first[&company].date, second[&company].date);
    assert_eq!(first[&company].start_utc, second[&company].start_utc);
    assert!(
        erp_addon_base::company_calendar::business_days(&mut tx, &env, &[])
            .await
            .unwrap()
            .is_empty()
    );
    env.records("res.company")
        .unwrap()
        .browse([company])
        .unwrap()
        .write(
            &mut tx,
            RawVals::new().set_cell("tz", text("Pacific/Kiritimati")),
        )
        .await
        .unwrap();
    let changed = erp_addon_base::company_calendar::business_days(&mut tx, &env, &[company])
        .await
        .unwrap();
    let expected: erp_fields::Date = tx
        .query(
            "SELECT (transaction_timestamp() AT TIME ZONE 'Pacific/Kiritimati')::date",
            &[],
        )
        .await
        .unwrap()[0]
        .get(0);
    assert_eq!(changed[&company].date, expected);
    let error = erp_addon_base::company_calendar::business_days(&mut tx, &env, &[i64::MAX])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Missing);
    assert_eq!(
        error.message(),
        format!("Company {} is unavailable.", i64::MAX)
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn company_calendar_denies_zone_field_before_raw_lookup() {
    let policy = erp_testkit::FieldPolicy::new().deny("res.company", "tz", erp_sdk::FieldOp::Read);
    let h = Harness::builder([&erp_addon_base::ADDON])
        .policy(policy.clone())
        .build()
        .await
        .unwrap();
    policy.attach(h.registry()).unwrap();
    let mut tx = h.begin().await.unwrap();
    let currency = currency(&mut tx).await;
    let company = create(
        &mut tx,
        "res.company",
        RawVals::new()
            .set_cell("name", text("Field restricted"))
            .set_cell("currency_id", Cell::Id(currency)),
    )
    .await;
    // FieldPolicy is an isolated permission fixture, with no user resolver or installed data.
    let caller = tx
        .env()
        .records("res.company")
        .unwrap()
        .with_user(erp_orm::UserId::new(2).unwrap());
    let error = erp_addon_base::company_calendar::business_days(&mut tx, caller.env(), &[company])
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Access);
    assert_eq!(
        error.message(),
        "Reading the company time zone is not allowed."
    );
    tx.rollback().await.unwrap();
    drop(tx);
    h.cleanup().await.unwrap();
}
