// SPDX-License-Identifier: MIT OR Apache-2.0
//! Product pictures through an installed database (task IMG.BIN-SIZE-AND-PRODUCT): the template's
//! image family from `image.mixin`, the variant's own picture and its stored sizes, the fallback
//! of a variant to its template's picture, where a picture written on a variant is kept, and the
//! move of a deleted variant's picture to a template that has none.
use super::*;
use erp_fields::Value;
use erp_orm::Transaction;
use erp_sdk::ErrorKind;
use erp_sdk::image::{ImageFormat, probe};
use std::collections::BTreeMap;

/// A solid-colour 400 x 200 PNG (8-bit RGB, 574 bytes), as standard padded base64. Made with
/// Python's `zlib` and `struct`: signature, IHDR, one IDAT, IEND.
const PNG_400X200: &str = "iVBORw0KGgoAAAANSUhEUgAAAZAAAADICAIAAABJdyC1AAACBUlEQVR42u3UMQ0AAAgEsXeENNThDxUMJE2q4IZL9QC8EAkAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMCzAsFQDDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLMCwVAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCwAwwIwLMCwAAwLwLAAwwIwLADDAgwLwLAADAswLADDAjAswLAADAswLADDAjAswLAADAvAsADDAjAsAMMCDAvAsAAMCzAsAMMCMCzAsAAMC8CwAMMCMCwAwwIMC8CwAAwLMCyAOwvzlum0ON75nAAAAABJRU5ErkJggg==";

/// A solid-colour 200 x 400 PNG (8-bit RGB, 776 bytes) in another colour, as standard padded
/// base64. Made the same way; its upright shape tells it apart from [`PNG_400X200`] in every size.
const PNG_200X400: &str = "iVBORw0KGgoAAAANSUhEUgAAAMgAAAGQCAIAAABkkLjnAAACz0lEQVR42u3SQQkAAAgEwUtiTiMayxKCn4FJsGyqB85FAoyFsTAWGAtjYSwwFsbCWGAsjIWxwFgYC2OBsTAWxgJjYSyMBcbCWBgLjIWxMBYYC2NhLDAWxsJYYCyMhbHAWBgLY4GxMBbGAmNhLIwFxsJYGAuMhbEwFhgLY2EsMBbGwlhgLIyFscBYGAtjgbEwFsYCY2EsjAXGwlgYC4yFsTAWGAtjYSwwFsbCWGAsjIWxwFgYC2NhLBUwFsbCWGAsjIWxwFgYC2OBsTAWxgJjYSyMBcbCWBgLjIWxMBYYC2NhLDAWxsJYYCyMhbHAWBgLY4GxMBbGAmNhLIwFxsJYGAuMhbEwFhgLY2EsMBbGwlhgLIyFscBYGAtjgbEwFsYCY2EsjAXGwlgYC4yFsTAWGAtjYSwwFsbCWGAsjIWxwFgYC2OBsTAWxsJYKmAsjIWxwFgYC2OBsTAWxgJjYSyMBcbCWBgLjIWxMBYYC2NhLDAWxsJYYCyMhbHAWBgLY4GxMBbGAmNhLIwFxsJYGAuMhbEwFhgLY2EsMBbGwlhgLIyFscBYGAtjgbEwFsYCY2EsjAXGwlgYC4yFsTAWGAtjYSwwFsbCWGAsjIWxwFgYC2OBsTAWxgJjYSyMhbEkwFgYC2OBsTAWxgJjYSyMBcbCWBgLjIWxMBYYC2NhLDAWxsJYYCyMhbHAWBgLY4GxMBbGAmNhLIwFxsJYGAuMhbEwFhgLY2EsMBbGwlhgLIyFscBYGAtjgbEwFsYCY2EsjAXGwlgYC4yFsTAWGAtjYSwwFsbCWGAsjIWxwFgYC2OBsTAWxgJjYSyMBcbCWBgLjIWxMBbGUgFjYSyMBcbCWBgLjIWxMBYYC2NhLDAWxsJYYCyMhbHAWBgLY4GxMBbGAmNhLIwFxsJYGAuMhbEwFhgLY2EsMBbGwlhgLIyFscBYGAtjgbEwFsYCY2EsjAXG4sMCe9tYBmsw9BoAAAAASUVORK5CYII=";

/// The text `not an image`, as standard padded base64.
const NOT_AN_IMAGE: &str = "bm90IGFuIGltYWdl";

/// The five sizes of the shown picture, largest first.
const SIZES: [&str; 5] = [
    "image_1920",
    "image_1024",
    "image_512",
    "image_256",
    "image_128",
];

/// The five sizes of a variant's own picture, in the order of [`SIZES`].
const VARIANT_SIZES: [&str; 5] = [
    "image_variant_1920",
    "image_variant_1024",
    "image_variant_512",
    "image_variant_256",
    "image_variant_128",
];

/// Header sizes of [`PNG_400X200`] stored in each of [`SIZES`]: kept under the bound, scaled down
/// with the ratio kept above it.
const WIDE: [(u32, u32); 5] = [(400, 200), (400, 200), (400, 200), (256, 128), (128, 64)];

/// Header sizes of [`PNG_200X400`] stored in each of [`SIZES`].
const TALL: [(u32, u32); 5] = [(200, 400), (200, 400), (200, 400), (128, 256), (64, 128)];

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

/// Write one field the way an RPC client does: through the dynamic value converter, base64 text
/// for content and `false` to clear.
async fn write_value(
    tx: &mut Transaction,
    model: &str,
    id: i64,
    field: &str,
    value: Value,
) -> erp_sdk::prelude::Result<()> {
    let record = tx.env().records(model).unwrap().browse([id]).unwrap();
    let vals = record.vals_from_values(&BTreeMap::from([(field.to_owned(), value)]))?;
    record.write(tx, vals).await
}

/// Format and header size of the picture a field holds, read by our own `probe`.
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
        other => panic!("{model}.{field} of {id} is {other:?}, expected bytes"),
    }
}

/// Every size of `fields` on the record holds a PNG of the expected header size.
async fn assert_sizes(
    tx: &mut Transaction,
    model: &str,
    id: i64,
    fields: [&str; 5],
    expected: [(u32, u32); 5],
) {
    for (field, (width, height)) in fields.into_iter().zip(expected) {
        assert_eq!(
            stored(tx, model, id, field).await,
            (ImageFormat::Png, width, height),
            "{model}.{field} of {id}"
        );
    }
}

/// Every field of `fields` on the record is empty.
async fn assert_empty(tx: &mut Transaction, model: &str, id: i64, fields: [&str; 5]) {
    for field in fields {
        assert_eq!(
            cell(tx, model, id, field).await,
            Cell::Null,
            "{model}.{field} of {id}"
        );
    }
}

/// The variant shows, in every size, exactly the content its template holds in that size.
async fn assert_shows_template(tx: &mut Transaction, variant: i64, template: i64) {
    for field in SIZES {
        let shown = cell(tx, "product.product", variant, field).await;
        let held = cell(tx, "product.template", template, field).await;
        assert_eq!(shown, held, "product.product.{field} of {variant}");
    }
}

/// The variant shows, in every size, exactly its own stored picture of that size.
async fn assert_shows_own(tx: &mut Transaction, variant: i64) {
    for (field, own) in SIZES.into_iter().zip(VARIANT_SIZES) {
        let shown = cell(tx, "product.product", variant, field).await;
        let held = cell(tx, "product.product", variant, own).await;
        assert!(
            matches!(held, Cell::Bytes(_)),
            "product.product.{own} of {variant} is {held:?}"
        );
        assert_eq!(shown, held, "product.product.{field} of {variant}");
    }
}

/// A new template and the variant its creation made.
async fn template(tx: &mut Transaction, name: &str) -> (i64, i64) {
    let template = tx
        .env()
        .records("product.template")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str(name.into()))],
        )
        .await
        .unwrap()
        .ids()[0];
    let variants = variants(tx, template).await;
    assert_eq!(variants.len(), 1, "a new template has one variant");
    (template, variants[0])
}

async fn variants(tx: &mut Transaction, template: i64) -> Vec<i64> {
    tx.query(
        "SELECT id FROM product_product WHERE product_tmpl_id=$1 ORDER BY id",
        &[&template],
    )
    .await
    .unwrap()
    .iter()
    .map(|row| row.get(0))
    .collect()
}

/// A second active variant of `template`.
async fn second_variant(tx: &mut Transaction, template: i64) -> i64 {
    let variant = tx
        .env()
        .records("product.product")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("product_tmpl_id", Cell::Id(template))],
        )
        .await
        .unwrap()
        .ids()[0];
    assert_eq!(variants(tx, template).await.len(), 2);
    variant
}

async fn unlink(tx: &mut Transaction, model: &str, id: i64) {
    tx.env()
        .records(model)
        .unwrap()
        .browse([id])
        .unwrap()
        .unlink(tx)
        .await
        .unwrap();
}

/// the case are not covered here)
#[test]
fn product_image_falls_back_from_the_variant_to_its_template() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let (product, first) = template(&mut tx, "Picture fallback").await;

        // a new product has no picture, and neither has its variant
        assert_empty(&mut tx, "product.template", product, SIZES).await;
        assert_empty(&mut tx, "product.product", first, SIZES).await;
        assert_empty(&mut tx, "product.product", first, VARIANT_SIZES).await;

        // the template picture fills the four stored sizes
        write_value(
            &mut tx,
            "product.template",
            product,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;

        // a variant without its own picture shows the template's, in every size, in the same
        // transaction
        assert_empty(&mut tx, "product.product", first, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, first, product).await;
        assert_sizes(&mut tx, "product.product", first, SIZES, WIDE).await;

        // with two active variants a picture written on one stays on that variant
        let second = second_variant(&mut tx, product).await;
        assert_shows_template(&mut tx, second, product).await;
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.product", second, VARIANT_SIZES, TALL).await;
        assert_shows_own(&mut tx, second).await;
        // the template and the other variant keep the template picture
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;
        assert_empty(&mut tx, "product.product", first, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, first, product).await;

        // a changed template picture shows on the variant without its own, not on the other
        write_value(
            &mut tx,
            "product.template",
            product,
            "image_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.product", first, SIZES, TALL).await;
        assert_shows_template(&mut tx, first, product).await;
        assert_shows_own(&mut tx, second).await;

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// Where a picture written on a variant is kept, and what clearing it clears (the inverse of
/// `product.product.image_1920`). No spec case names these rules yet, so this test carries no
/// case tag.
#[test]
fn product_image_written_on_a_variant_goes_to_the_template_or_the_variant() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();

        // one variant: the picture always lands on the template
        let (single, only) = template(&mut tx, "Single variant picture").await;
        write_value(
            &mut tx,
            "product.product",
            only,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.template", single, SIZES, WIDE).await;
        assert_empty(&mut tx, "product.product", only, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, only, single).await;
        // also when the template already has one
        write_value(
            &mut tx,
            "product.product",
            only,
            "image_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.template", single, SIZES, TALL).await;
        assert_empty(&mut tx, "product.product", only, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, only, single).await;
        // and clearing it on the variant clears the template
        write_value(&mut tx, "product.product", only, "image_1920", Value::False)
            .await
            .unwrap();
        assert_empty(&mut tx, "product.template", single, SIZES).await;
        assert_empty(&mut tx, "product.product", only, SIZES).await;

        // two active variants, template without a picture: the first picture written on a
        // variant goes to the template
        let (product, first) = template(&mut tx, "Two variant picture").await;
        let second = second_variant(&mut tx, product).await;
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;
        assert_empty(&mut tx, "product.product", second, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, first, product).await;
        assert_shows_template(&mut tx, second, product).await;

        // the template has one now: the next picture stays on the variant
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.product", second, VARIANT_SIZES, TALL).await;
        assert_shows_own(&mut tx, second).await;
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;

        // clearing a variant that has its own picture clears the variant only: it shows the
        // template's again
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::False,
        )
        .await
        .unwrap();
        assert_empty(&mut tx, "product.product", second, VARIANT_SIZES).await;
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;
        assert_shows_template(&mut tx, second, product).await;

        // clearing a variant that has no own picture clears the template, for every variant
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::False,
        )
        .await
        .unwrap();
        assert_empty(&mut tx, "product.template", product, SIZES).await;
        assert_empty(&mut tx, "product.product", first, SIZES).await;
        assert_empty(&mut tx, "product.product", second, SIZES).await;
        assert_empty(&mut tx, "product.product", second, VARIANT_SIZES).await;

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// A deleted variant's own picture moves to a template that has none, and never replaces a
/// template picture. No spec case names this rule yet, so this test carries no case tag.
#[test]
fn product_image_of_a_deleted_variant_moves_to_a_template_without_one() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();

        // template without a picture: the variant's picture moves to it
        let (product, first) = template(&mut tx, "Deleted variant picture").await;
        let second = second_variant(&mut tx, product).await;
        write_value(
            &mut tx,
            "product.product",
            second,
            "image_variant_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        assert_sizes(&mut tx, "product.product", second, VARIANT_SIZES, TALL).await;
        assert_empty(&mut tx, "product.template", product, SIZES).await;
        assert_empty(&mut tx, "product.product", first, SIZES).await;
        unlink(&mut tx, "product.product", second).await;
        assert_eq!(variants(&mut tx, product).await, vec![first]);
        assert_sizes(&mut tx, "product.template", product, SIZES, TALL).await;
        assert_shows_template(&mut tx, first, product).await;

        // template with a picture: it keeps its own
        let (kept, _) = template(&mut tx, "Kept template picture").await;
        let extra = second_variant(&mut tx, kept).await;
        write_value(
            &mut tx,
            "product.template",
            kept,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        write_value(
            &mut tx,
            "product.product",
            extra,
            "image_variant_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        unlink(&mut tx, "product.product", extra).await;
        assert_sizes(&mut tx, "product.template", kept, SIZES, WIDE).await;

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// Content that is not a picture is refused by name, by the field it is written on (the template's
/// picture, the variant's shown picture, the variant's own picture), and nothing is stored. No
/// spec case names the refusal sentence yet, so this test carries no case tag.
#[test]
fn product_image_refuses_content_that_is_not_a_picture() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let (product, variant) = template(&mut tx, "Refused picture").await;
        tx.commit().await.unwrap();

        // (written model, record, written field, the field the refusal names); a refused write
        // ends its transaction, as a refused request does
        for (model, id, field, named) in [
            (
                "product.template",
                product,
                "image_1920",
                "product.template.image_1920",
            ),
            (
                "product.product",
                variant,
                "image_1920",
                "product.product.image_1920",
            ),
            (
                "product.product",
                variant,
                "image_variant_1920",
                "product.product.image_variant_1920",
            ),
        ] {
            let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
            let error = write_value(&mut tx, model, id, field, Value::from(NOT_AN_IMAGE))
                .await
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
            assert_eq!(
                error.message(),
                format!(
                    "Invalid image for {named}: the file is not a PNG, JPEG, GIF or WebP image"
                ),
                "{model}.{field}"
            );
            tx.rollback().await.unwrap();
        }

        // nothing was stored
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_empty(&mut tx, "product.template", product, SIZES).await;
        assert_empty(&mut tx, "product.product", variant, SIZES).await;
        assert_empty(&mut tx, "product.product", variant, VARIANT_SIZES).await;

        // the same on a template with a picture and two variants, where a variant's picture is
        // kept on the variant: both pictures stay as they were
        let second = second_variant(&mut tx, product).await;
        write_value(
            &mut tx,
            "product.template",
            product,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let error = write_value(
            &mut tx,
            "product.product",
            second,
            "image_1920",
            Value::from(NOT_AN_IMAGE),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{}", error.message());
        assert_eq!(
            error.message(),
            "Invalid image for product.product.image_1920: the file is not a PNG, JPEG, GIF or WebP image"
        );
        tx.rollback().await.unwrap();
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_empty(&mut tx, "product.product", second, VARIANT_SIZES).await;
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;
        assert_shows_template(&mut tx, second, product).await;

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// The bytes a field holds, read inside `tx`.
async fn bytes(tx: &mut Transaction, model: &str, id: i64, field: &str) -> Vec<u8> {
    match cell(tx, model, id, field).await {
        Cell::Bytes(content) => content.to_vec(),
        other => panic!("{model}.{field} of {id} is {other:?}, expected bytes"),
    }
}

/// One JSON-2 call that must answer 200; the parsed body.
async fn json2(
    service: &RpcService,
    principal: &erp_orm::Principal,
    model: &str,
    method: &str,
    args: serde_json::Value,
) -> serde_json::Value {
    let response = service
        .call(RpcCall::json2(
            &format!("/json/2/{model}/{method}"),
            serde_json::to_vec(&args).unwrap(),
            Caller::Principal(principal.clone()),
        ))
        .await;
    assert_eq!(
        response.status(),
        200,
        "{model}/{method}: {}",
        String::from_utf8_lossy(response.body())
    );
    serde_json::from_slice(response.body()).unwrap()
}

/// picture and of a computed one, the same read without it the base64 content; a save and an
/// onchange that do not name the picture leave it stored, which is what the web client relies on
/// when it reads its views with `bin_size`)
#[test]
fn product_image_reads_as_a_size_with_bin_size_over_json2() {
    use erp_orm::image::human_size;

    big_stack(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let (_system, installed) = install(&db).await;
        let main = xid(&installed, "base", "main_company").await;
        let system = xid(&installed, "base", "group_system").await;
        let manager = xid(&installed, "product", "group_product_manager").await;
        let mut tx = installed.begin(SUPERUSER_ID).await.unwrap();
        // the calls below run as a product manager, a real user: JSON-2 refuses the system principal
        access::user(&mut tx, "picture-bootstrap", main, &[system]).await;
        access::user(&mut tx, "picture-manager", main, &[manager]).await;
        let (product, variant) = template(&mut tx, "Sized picture").await;
        write_value(
            &mut tx,
            "product.template",
            product,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        // what the fields hold, from reads of the content
        let full = bytes(&mut tx, "product.template", product, "image_1920").await;
        let small = bytes(&mut tx, "product.template", product, "image_128").await;
        assert!(small.len() < full.len(), "the 128 size is the smaller file");
        tx.commit().await.unwrap();
        drop(tx);
        let orm = access::secure_orm(&db, &installed).await;
        let mut audit = orm
            .begin_opts(
                erp_orm::BeginOptions::new(SUPERUSER_ID)
                    .isolation(erp_orm::Isolation::ReadCommitted)
                    .on_primary(),
            )
            .await
            .unwrap();
        audit.install_audit_schema().await.unwrap();
        audit.commit().await.unwrap();
        drop(audit);
        let principal = access::authenticated(&orm, "picture-manager").await;
        let service = RpcService::new(orm.clone(), Host, RpcConfig::default());
        let sized = serde_json::json!({"bin_size": true});

        // the stored fields of the template
        let rows = json2(
            &service,
            &principal,
            "product.template",
            "read",
            serde_json::json!({"ids": [product], "fields": ["image_1920", "image_128"], "context": sized}),
        )
        .await;
        assert_eq!(rows[0]["image_1920"], human_size(full.len()), "{rows}");
        assert_eq!(rows[0]["image_128"], human_size(small.len()), "{rows}");

        // the computed fields of the variant (the template's picture), and its empty own picture
        let found = json2(
            &service,
            &principal,
            "product.product",
            "web_search_read",
            serde_json::json!({
                "domain": [["id", "=", variant]],
                "specification": {"image_1920": {}, "image_128": {}, "image_variant_1920": {}},
                "context": sized,
            }),
        )
        .await;
        let row = &found["records"][0];
        assert_eq!(row["image_1920"], human_size(full.len()), "{found}");
        assert_eq!(row["image_128"], human_size(small.len()), "{found}");
        assert_eq!(row["image_variant_1920"], false, "{found}");

        // without `bin_size` the same read carries the content: what the image widget asks for
        let rows = json2(
            &service,
            &principal,
            "product.product",
            "read",
            serde_json::json!({"ids": [variant], "fields": ["image_128"], "context": {"bin_size": false}}),
        )
        .await;
        assert_eq!(
            rows[0]["image_128"],
            erp_fields::base64::encode(&small),
            "{rows}"
        );

        // a save that does not name the picture answers its size and leaves it stored
        let saved = json2(
            &service,
            &principal,
            "product.template",
            "web_save",
            serde_json::json!({
                "ids": [product],
                "vals": {"name": "Sized picture, renamed"},
                "specification": {"name": {}, "image_1920": {}},
                "context": sized,
            }),
        )
        .await;
        assert_eq!(saved[0]["name"], "Sized picture, renamed", "{saved}");
        assert_eq!(saved[0]["image_1920"], human_size(full.len()), "{saved}");

        // an onchange whose values leave the picture out takes it from the stored record: the
        // answer does not change it
        let changed = json2(
            &service,
            &principal,
            "product.template",
            "onchange",
            serde_json::json!({
                "ids": [product],
                "values": {"name": "Sized picture, typed"},
                "field_names": ["name"],
                "fields_spec": {"name": {}, "image_1920": {}, "image_128": {}},
            }),
        )
        .await;
        let value = changed["value"].as_object().unwrap();
        assert!(!value.contains_key("image_1920"), "{changed}");
        assert!(!value.contains_key("image_128"), "{changed}");

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            bytes(&mut tx, "product.template", product, "image_1920").await,
            full
        );
        assert_eq!(
            bytes(&mut tx, "product.product", variant, "image_128").await,
            small
        );
        tx.rollback().await.unwrap();
        drop(tx);
        db.cleanup().await.unwrap();
    });
}

/// Write one field on several records in one call, the way an RPC client does.
async fn write_many(tx: &mut Transaction, model: &str, ids: &[i64], field: &str, value: Value) {
    let records = tx
        .env()
        .records(model)
        .unwrap()
        .browse(ids.iter().copied())
        .unwrap();
    let vals = records
        .vals_from_values(&BTreeMap::from([(field.to_owned(), value)]))
        .unwrap();
    records.write(tx, vals).await.unwrap();
}

/// A further active variant of `template`, whatever the number it already has.
async fn another_variant(tx: &mut Transaction, template: i64) -> i64 {
    tx.env()
        .records("product.product")
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("product_tmpl_id", Cell::Id(template))],
        )
        .await
        .unwrap()
        .ids()[0]
}

/// The pictures of a write on two variants of one template, of a variant created with a picture,
/// of a delete of two variants and of a delete that is refused after the picture moved. No spec
/// case names these rules yet, so this test carries no case tag.
#[test]
fn product_image_moves_per_variant_and_a_refused_delete_moves_nothing() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();

        // one write on two variants of a template without a picture: the first fills the
        // template, the second keeps the picture on its variant (the template is read again
        // per variant)
        let (product, first) = template(&mut tx, "Two in one write").await;
        let second = second_variant(&mut tx, product).await;
        let third = another_variant(&mut tx, product).await;
        write_many(
            &mut tx,
            "product.product",
            &[first, second],
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await;
        assert_sizes(&mut tx, "product.template", product, SIZES, WIDE).await;
        assert_empty(&mut tx, "product.product", first, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, first, product).await;
        assert_sizes(&mut tx, "product.product", second, VARIANT_SIZES, WIDE).await;
        assert_empty(&mut tx, "product.product", third, VARIANT_SIZES).await;

        // a variant created with a picture, on a template without one: the picture goes to the
        // template
        let (created_on, _) = template(&mut tx, "Created with a picture").await;
        let vals = tx
            .env()
            .records("product.product")
            .unwrap()
            .vals_from_values(&BTreeMap::from([
                ("product_tmpl_id".to_owned(), Value::Int(created_on)),
                ("image_1920".to_owned(), Value::from(PNG_200X400)),
            ]))
            .unwrap();
        let created = tx
            .env()
            .records("product.product")
            .unwrap()
            .create(&mut tx, vec![vals])
            .await
            .unwrap()
            .ids()[0];
        assert_sizes(&mut tx, "product.template", created_on, SIZES, TALL).await;
        assert_empty(&mut tx, "product.product", created, VARIANT_SIZES).await;
        assert_shows_template(&mut tx, created, created_on).await;

        // two variants with pictures deleted in one call: the first fills the template
        let (bare, kept) = template(&mut tx, "Two deleted").await;
        let tall = second_variant(&mut tx, bare).await;
        let wide = another_variant(&mut tx, bare).await;
        for (id, picture) in [(tall, PNG_200X400), (wide, PNG_400X200)] {
            write_value(
                &mut tx,
                "product.product",
                id,
                "image_variant_1920",
                Value::from(picture),
            )
            .await
            .unwrap();
        }
        tx.env()
            .records("product.product")
            .unwrap()
            .browse([tall, wide])
            .unwrap()
            .unlink(&mut tx)
            .await
            .unwrap();
        assert_eq!(variants(&mut tx, bare).await, vec![kept]);
        assert_sizes(&mut tx, "product.template", bare, SIZES, TALL).await;

        // a delete refused after the picture moved (a combo item holds the variant): the
        // template stays without a picture and the variant keeps its own
        let (refused, _) = template(&mut tx, "Refused delete").await;
        let held = second_variant(&mut tx, refused).await;
        write_value(
            &mut tx,
            "product.product",
            held,
            "image_variant_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        let combo = tx
            .env()
            .records("product.combo")
            .unwrap()
            .create(
                &mut tx,
                vec![RawVals::new().set_cell("name", Cell::Str("Picture combo".into()))],
            )
            .await
            .unwrap()
            .ids()[0];
        tx.env()
            .records("product.combo.item")
            .unwrap()
            .create(
                &mut tx,
                vec![
                    RawVals::new()
                        .set_cell("combo_id", Cell::Id(combo))
                        .set_cell("product_id", Cell::Id(held)),
                ],
            )
            .await
            .unwrap();
        let error = tx
            .env()
            .records("product.product")
            .unwrap()
            .browse([held])
            .unwrap()
            .unlink(&mut tx)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Validation, "{error}");
        assert_eq!(variants(&mut tx, refused).await.len(), 2);
        assert_empty(&mut tx, "product.template", refused, SIZES).await;
        assert_sizes(&mut tx, "product.product", held, VARIANT_SIZES, TALL).await;

        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// The variants of several templates read the templates' pictures in one statement, not one per
/// template (semantic statement contract, ceiling: the reference reads them in one
/// prefetch). No spec case names this yet, so this test carries no case tag.
#[test]
fn product_image_of_many_variants_reads_their_templates_once() {
    big_stack(async {
        let db = TestDb::create().await.unwrap();
        let (_system, orm) = install(&db).await;
        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let mut shown = Vec::new();
        for name in ["Page one", "Page two", "Page three"] {
            let (product, variant) = template(&mut tx, name).await;
            write_value(
                &mut tx,
                "product.template",
                product,
                "image_1920",
                Value::from(PNG_400X200),
            )
            .await
            .unwrap();
            shown.push(variant);
        }
        tx.commit().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        let page = tx
            .env()
            .records("product.product")
            .unwrap()
            .browse(shown.iter().copied())
            .unwrap();
        tx.capture_statements(true);
        let cells = page.cells(&mut tx, "image_128").await.unwrap();
        let statements = tx.captured().to_vec();
        tx.capture_statements(false);
        assert!(
            cells.iter().all(|cell| matches!(cell, Cell::Bytes(_))),
            "{cells:?}"
        );
        let template_reads = statements
            .iter()
            .filter(|sql| sql.contains("product_template") && sql.contains("\"image_128\""))
            .count();
        assert_eq!(template_reads, 1, "{statements:#?}");
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}

/// Pictures read and written by restricted users: an internal user of another company reads no
/// picture of a variant of a company-bound template, sized or not; a user who may only read
/// changes and deletes nothing, and the template keeps its picture; that user's sized read of a
/// stored picture sends no statement that fetches the content. No spec case names these rules
/// yet, so this test carries no case tag.
#[test]
fn product_image_access_holds_for_restricted_users() {
    use erp_orm::image::human_size;

    big_stack(async {
        let db = TestDb::builder().pool_size(8).create().await.unwrap();
        let (_system, installed) = install(&db).await;
        let main = xid(&installed, "base", "main_company").await;
        let system = xid(&installed, "base", "group_system").await;
        let internal = xid(&installed, "base", "group_user").await;
        let mut setup = installed.begin(SUPERUSER_ID).await.unwrap();
        access::user(&mut setup, "picture-access-bootstrap", main, &[system]).await;
        let companies = setup.env().records("res.company").unwrap();
        let currency = companies
            .browse([main])
            .unwrap()
            .cell(&mut setup, "currency_id")
            .await
            .unwrap();
        let other = companies
            .create(
                &mut setup,
                vec![
                    RawVals::new()
                        .set_cell("name", Cell::Str("Picture company X".into()))
                        .set_cell("currency_id", currency),
                ],
            )
            .await
            .unwrap()
            .ids()[0];
        let outsider = access::user(&mut setup, "picture-outsider", other, &[internal]).await;
        let reader = access::user(&mut setup, "picture-reader", main, &[internal]).await;
        // a template of the main company with a picture and two variants, one with its own
        let (product, plain) = template(&mut setup, "Company picture").await;
        let own = second_variant(&mut setup, product).await;
        setup
            .env()
            .records("product.template")
            .unwrap()
            .browse([product])
            .unwrap()
            .write(
                &mut setup,
                RawVals::new().set_cell("company_id", Cell::Id(main)),
            )
            .await
            .unwrap();
        write_value(
            &mut setup,
            "product.template",
            product,
            "image_1920",
            Value::from(PNG_400X200),
        )
        .await
        .unwrap();
        write_value(
            &mut setup,
            "product.product",
            own,
            "image_variant_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap();
        let full = bytes(&mut setup, "product.template", product, "image_1920").await;
        let held = bytes(&mut setup, "product.product", own, "image_variant_1920").await;
        setup.commit().await.unwrap();
        drop(setup);
        let orm = access::secure_orm(&db, &installed).await;

        // another company: every picture of the variant is refused, sized or not
        for field in ["image_128", "image_1920", "image_variant_1920"] {
            for sized in [false, true] {
                let mut tx = orm.begin(UserId::new(outsider).unwrap()).await.unwrap();
                let variant = tx
                    .env()
                    .records("product.product")
                    .unwrap()
                    .browse([own])
                    .unwrap()
                    .with_context([("bin_size", Value::Bool(sized))]);
                let error = variant.read(&mut tx, &[field]).await.unwrap_err();
                assert_eq!(
                    error.kind(),
                    ErrorKind::Access,
                    "{field} sized={sized}: {error}"
                );
                tx.rollback().await.unwrap();
            }
        }

        // read only: the sized read answers the size from PostgreSQL, never the content
        let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
        let sized = |model: &str, id: i64, tx: &Transaction| {
            tx.env()
                .records(model)
                .unwrap()
                .browse([id])
                .unwrap()
                .with_context([("bin_size", Value::Bool(true))])
        };
        tx.capture_statements(true);
        let template_rows = sized("product.template", product, &tx)
            .read(&mut tx, &["image_1920"])
            .await
            .unwrap();
        let variant_rows = sized("product.product", own, &tx)
            .read(&mut tx, &["image_variant_1920"])
            .await
            .unwrap();
        let statements = tx.captured().to_vec();
        tx.capture_statements(false);
        assert_eq!(
            template_rows[0]["image_1920"],
            Value::str(human_size(full.len()).as_str())
        );
        assert_eq!(
            variant_rows[0]["image_variant_1920"],
            Value::str(human_size(held.len()).as_str())
        );
        for column in ["\"image_1920\"", "\"image_variant_1920\""] {
            let naming: Vec<&String> = statements
                .iter()
                .filter(|sql| sql.contains(column))
                .collect();
            assert!(
                !naming.is_empty(),
                "{column} is sized by a statement: {statements:#?}"
            );
            for sql in naming {
                assert!(
                    sql.contains(&format!("octet_length({column})")),
                    "a sized read fetched {column}: {sql}"
                );
            }
        }
        tx.rollback().await.unwrap();
        drop(tx);

        // read only: a picture written on a variant is refused and the template keeps its own
        let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
        let error = write_value(
            &mut tx,
            "product.product",
            plain,
            "image_1920",
            Value::from(PNG_200X400),
        )
        .await
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{error}");
        tx.rollback().await.unwrap();
        drop(tx);

        // read only: the delete is refused for the delete itself, before any picture moves
        let mut tx = orm.begin(UserId::new(reader).unwrap()).await.unwrap();
        let error = tx
            .env()
            .records("product.product")
            .unwrap()
            .browse([own])
            .unwrap()
            .unlink(&mut tx)
            .await
            .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Access, "{error}");
        assert!(
            error.message().contains("delete") && error.message().contains("(product.product)"),
            "{error}"
        );
        tx.rollback().await.unwrap();
        drop(tx);

        let mut tx = orm.begin(SUPERUSER_ID).await.unwrap();
        assert_eq!(
            bytes(&mut tx, "product.template", product, "image_1920").await,
            full
        );
        assert_eq!(
            bytes(&mut tx, "product.product", own, "image_variant_1920").await,
            held
        );
        assert_eq!(variants(&mut tx, product).await, vec![plain, own]);
        tx.rollback().await.unwrap();
        drop(tx);
        drop(orm);
        db.cleanup().await.unwrap();
    });
}
