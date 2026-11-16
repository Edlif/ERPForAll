// SPDX-License-Identifier: MIT OR Apache-2.0
//! The company logo through an initialized database (task IMG.FIELD): `res.company.logo` is the
//! picture of the company's contact (`res.partner.image_1920`, from `image.mixin`), the contact's
//! stored sizes and the company's `logo_web` follow a write, and content that is not a picture is
//! refused.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use erp_sdk::image::{ImageFormat, probe};
use erp_sdk::prelude::*;
use erp_sdk::{ErrorKind, SUPERUSER_ID};
use std::collections::BTreeMap;

/// A solid-colour 400 x 200 PNG (8-bit RGB, 574 bytes), as standard padded base64. Made with
/// Python's `zlib` and `struct`: signature, IHDR, one IDAT, IEND.
const PNG_400X200: &str = "iVBORw0KGgoAAAANSUhEUgAAAZAAAADICAIAAABJdyC1AAACBUlEQVR42u3UMQ0AAAgEsXeENNThDxUMJE2q4IZL9QC8EAkAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMCzAsFQDDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLMCwVAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCyAOwvzlum0ON75nAAAAABJRU5ErkJggg==";

/// The text `not an image`, as standard padded base64.
const NOT_AN_IMAGE: &str = "bm90IGFuIGltYWdl";

/// A fresh database with `base` installed and its data loaded (the main company and its contact).
async fn initialized(
    db: &erp_testkit::TestDb,
) -> (erp_modules::system::ModuleSystem, erp_orm::Orm) {
    let mut index = erp_modules::AddonIndex::new(true);
    index.add_dev_path(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../addons")
            .canonicalize()
            .unwrap(),
    );
    let native_artifact_identities = index
        .discover_entries()
        .unwrap()
        .into_iter()
        .map(|(name, entry)| {
            let snapshot = erp_modules::artifact_identity::ArtifactSnapshot::capture(
                name.as_str(),
                std::path::Path::new(entry.source_location()),
            )
            .unwrap();
            (name, snapshot.digest().to_owned())
        })
        .collect();
    let system = erp_modules::system::ModuleSystem::builder(db.pool().clone(), index)
        .native_artifact_identities(native_artifact_identities)
        .code([&erp_addon_base::ADDON, &erp_addon_uom::ADDON])
        .build()
        .unwrap();
    let init = Box::pin(
        system.initialize(erp_modules::system::InitOptions::new().with_skip_auto_install(true)),
    )
    .await
    .unwrap();
    let orm = init.orm;
    (system, orm)
}

/// The test future is large; the default 2 MiB test-thread stack overflows under the workspace
/// build.
fn big_stack<F: std::future::Future<Output = ()> + 'static>(test: fn() -> F) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap()
                .block_on(test());
        })
        .unwrap()
        .join()
        .unwrap();
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

/// Write `logo` on `company` the way an RPC client does: through the dynamic value converter,
/// base64 text for content and `false` to clear.
async fn write_logo(tx: &mut Transaction, company: i64, value: Value) -> Result<()> {
    let record = tx
        .env()
        .records("res.company")
        .unwrap()
        .browse([company])
        .unwrap();
    let vals = record.vals_from_values(&BTreeMap::from([("logo".to_owned(), value)]))?;
    record.write(tx, vals).await
}

/// Format and size of the stored picture, read from its header by our own `probe`.
async fn stored(
    tx: &mut Transaction,
    model: &str,
    id: i64,
    field: &str,
) -> (ImageFormat, u32, u32) {
    match cell(tx, model, id, field).await {
        Cell::Bytes(content) => {
            let info = probe(&content).unwrap_or_else(|error| {
                panic!("{model}.{field} holds no accepted picture: {error:?}")
            });
            (info.format, info.width, info.height)
        }
        other => panic!("{model}.{field} is {other:?}, expected bytes"),
    }
}

#[test]
fn company_logo_fills_the_contact_picture_and_its_sizes() {
    big_stack(sizes);
}

async fn sizes() {
    let db = erp_testkit::TestDb::create().await.unwrap();
    let (system, orm) = initialized(&db).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = xid(&mut tx, "base", "main_company", "res.company").await;
    let Cell::Id(partner) = cell(&mut tx, "res.company", company, "partner_id").await else {
        panic!("the main company has a contact");
    };

    // the main company ships without a logo
    assert_eq!(
        cell(&mut tx, "res.company", company, "logo").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.company", company, "logo_web").await,
        Cell::Null
    );
    assert_eq!(
        cell(&mut tx, "res.partner", partner, "image_1920").await,
        Cell::Null
    );

    write_logo(&mut tx, company, Value::from(PNG_400X200))
        .await
        .unwrap();

    // under 1920 pixels per edge: the size is kept
    assert_eq!(
        stored(&mut tx, "res.company", company, "logo").await,
        (ImageFormat::Png, 400, 200)
    );
    assert_eq!(
        stored(&mut tx, "res.partner", partner, "image_1920").await,
        (ImageFormat::Png, 400, 200)
    );
    // a size above the picture keeps its size, a size below it scales down with the ratio kept
    for (field, size) in [
        ("image_1024", (400, 200)),
        ("image_512", (400, 200)),
        ("image_256", (256, 128)),
        ("image_128", (128, 64)),
    ] {
        assert_eq!(
            stored(&mut tx, "res.partner", partner, field).await,
            (ImageFormat::Png, size.0, size.1),
            "{field}"
        );
    }
    // 180 pixels wide, any height
    assert_eq!(
        stored(&mut tx, "res.company", company, "logo_web").await,
        (ImageFormat::Png, 180, 90)
    );

    // `false` clears the logo, the contact's picture and every stored size
    write_logo(&mut tx, company, Value::False).await.unwrap();
    for field in ["logo", "logo_web"] {
        assert_eq!(
            cell(&mut tx, "res.company", company, field).await,
            Cell::Null,
            "{field}"
        );
    }
    for field in [
        "image_1920",
        "image_1024",
        "image_512",
        "image_256",
        "image_128",
    ] {
        assert_eq!(
            cell(&mut tx, "res.partner", partner, field).await,
            Cell::Null,
            "{field}"
        );
    }

    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    drop(system);
    db.cleanup().await.unwrap();
}

/// Content that is not a picture is refused by name and nothing is stored. No spec case names the
/// refusal sentence yet, so this test carries no case tag.
#[test]
fn company_logo_refuses_content_that_is_not_a_picture() {
    big_stack(refusal);
}

async fn refusal() {
    let db = erp_testkit::TestDb::create().await.unwrap();
    let (system, orm) = initialized(&db).await;
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    let company = xid(&mut tx, "base", "main_company", "res.company").await;

    let error = write_logo(&mut tx, company, Value::from(NOT_AN_IMAGE))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
    assert!(
        error
            .message()
            .starts_with("Invalid image for res.partner.image_1920")
            || error
                .message()
                .starts_with("Invalid image for res.company.logo"),
        "{}",
        error.message()
    );
    assert!(
        error
            .message()
            .contains("is not a PNG, JPEG, GIF or WebP image"),
        "{}",
        error.message()
    );
    tx.rollback().await.unwrap();
    drop(tx);

    // nothing was stored
    let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
    assert_eq!(
        cell(&mut tx, "res.company", company, "logo").await,
        Cell::Null
    );
    tx.rollback().await.unwrap();
    drop(tx);
    drop(orm);
    drop(system);
    db.cleanup().await.unwrap();
}
