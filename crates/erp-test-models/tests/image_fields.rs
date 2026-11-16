// SPDX-License-Identifier: MIT OR Apache-2.0
//! Binary and Image fields against the database (column-backed mode): the base64 write
//! and read forms, the `bytea` column, `bin_size`, image checks, scaling down to the field's
//! bounds, the stored related size, and the pixel limit as a system parameter.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use erp_orm::{Records, Transaction};
use erp_sdk::{Cell, ErrorKind, RawVals, Value};
use erp_test_models::test_image as fx;
use erp_testkit::Harness;

const MODEL: &str = "test_image.picture";

async fn harness() -> Harness {
    Harness::builder([&erp_addon_base::ADDON, &fx::ADDON])
        .build()
        .await
        .unwrap()
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A valid 8-bit RGB PNG of `width` x `height` with a colour gradient, written here byte by byte
/// (stored deflate blocks), so the fixture does not come from the encoder under test.
fn png(width: u32, height: u32) -> Vec<u8> {
    let mut raw = Vec::new();
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            raw.extend_from_slice(&[(x % 251) as u8, (y % 241) as u8, 90]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65_535).peekable();
    while let Some(block) = blocks.next() {
        let len = u16::try_from(block.len()).unwrap();
        zlib.push(u8::from(blocks.peek().is_none()));
        zlib.extend_from_slice(&len.to_le_bytes());
        zlib.extend_from_slice(&(!len).to_le_bytes());
        zlib.extend_from_slice(block);
    }
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &zlib);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn b64(bytes: &[u8]) -> Value {
    Value::str(erp_fields::base64::encode(bytes))
}

async fn create(tx: &mut Transaction, name: &str) -> Records {
    tx.env()
        .records(MODEL)
        .unwrap()
        .create(
            tx,
            vec![RawVals::new().set_cell("name", Cell::Str(name.into()))],
        )
        .await
        .unwrap()
}

/// Store `base.image_max_pixels` by SQL, whether or not the row of its declared default exists:
/// the test is about what the ORM reads, not about the parameter's own write checks.
async fn set_max_pixels(tx: &mut Transaction, value: &str) {
    set_parameter(tx, erp_orm::image::IMAGE_MAX_PIXELS_KEY, value).await;
}

/// Store one image parameter by SQL (see [`set_max_pixels`]).
async fn set_parameter(tx: &mut Transaction, key: &str, value: &str) {
    tx.execute(
        "WITH updated AS (UPDATE ir_config_parameter SET value = $2 WHERE key = $1 RETURNING 1) \
         INSERT INTO ir_config_parameter (key, value) SELECT $1, $2 \
         WHERE NOT EXISTS (SELECT 1 FROM updated)",
        &[&key, &value],
    )
    .await
    .unwrap();
}

/// The stored bytes of `field`, as a read that names the field returns them.
async fn content(tx: &mut Transaction, record: &Records, field: &str) -> Option<Vec<u8>> {
    let rows = record.read(tx, &[field]).await.unwrap();
    match rows[0].get(field).unwrap() {
        Value::False => None,
        Value::Bytes(bytes) => Some(bytes.to_vec()),
        other => panic!("{field} read as {other:?}"),
    }
}

/// Format and size of a stored picture, from our own probe.
async fn size(tx: &mut Transaction, record: &Records, field: &str) -> Option<(u32, u32)> {
    let bytes = content(tx, record, field).await?;
    let info = erp_orm::image::probe(&bytes).unwrap();
    assert_eq!(info.format, erp_orm::image::ImageFormat::Png, "{field}");
    Some((info.width, info.height))
}

/// file name half of the read object is not part of the column-backed slice)
#[tokio::test]
async fn binary_round_trips_through_base64_and_the_bytea_column() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "doc").await;
    let payload: Vec<u8> = (0..=255u8).cycle().take(5_000).collect();
    record
        .set_value(&mut tx, "document", b64(&payload))
        .await
        .unwrap();
    assert_eq!(
        content(&mut tx, &record, "document").await,
        Some(payload.clone())
    );
    let id = record.ids()[0];
    tx.commit().await.unwrap();

    // the column holds the bytes themselves, and a new transaction reads them from it
    let client = h.db().client().await.unwrap();
    let row = client
        .query_one(
            "SELECT document, pg_typeof(document)::text FROM test_image_picture WHERE id = $1",
            &[&id],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Vec<u8>>(0), payload);
    assert_eq!(row.get::<_, String>(1), "bytea");
    drop(client);

    // `bin_size` on values this transaction has not read: sized by the database
    let mut tx = h.begin().await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([id]).unwrap();
    let rows = record
        .with_context([("bin_size", Value::Bool(true))])
        .read(&mut tx, &["document", "original"])
        .await
        .unwrap();
    assert_eq!(rows[0].get("document"), Some(&Value::str("4.88 Kb")));
    assert_eq!(rows[0].get("original"), Some(&Value::False));
    drop(tx);

    let mut tx = h.begin().await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([id]).unwrap();
    assert_eq!(
        content(&mut tx, &record, "document").await,
        Some(payload.clone())
    );

    // a read without a field list never carries file content
    let all = record.read(&mut tx, &[]).await.unwrap();
    assert!(all[0].contains_key("name"));
    assert!(!all[0].contains_key("document"));
    assert!(!all[0].contains_key("image_1920"));

    // `bin_size`: the size as text in place of the content
    let sized = record.with_context([("bin_size", Value::Bool(true))]);
    let rows = sized
        .read(&mut tx, &["document", "original"])
        .await
        .unwrap();
    assert_eq!(rows[0].get("document"), Some(&Value::str("4.88 Kb")));
    assert_eq!(rows[0].get("original"), Some(&Value::False));

    // false clears; an empty string is no value either
    record
        .set_value(&mut tx, "document", Value::False)
        .await
        .unwrap();
    assert_eq!(content(&mut tx, &record, "document").await, None);
    record
        .set_value(&mut tx, "document", b64(b"x"))
        .await
        .unwrap();
    record
        .set_value(&mut tx, "document", Value::str(""))
        .await
        .unwrap();
    assert_eq!(content(&mut tx, &record, "document").await, None);
    drop(tx);
    h.cleanup().await.unwrap();
}

/// the content)
#[tokio::test]
async fn malformed_base64_is_refused_naming_the_field() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "doc").await;
    let error = record
        .set_value(&mut tx, "document", Value::str("not base64!"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid value for test_image.picture.document: the content is not valid base64 \
         (length 11 is not a multiple of 4)"
    );
    let error = record
        .set_value(&mut tx, "document", Value::Int(7))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid value for test_image.picture.document: expected base64 text or false, got an \
         integer"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// size follows its source on write, an unbounded field keeps the bytes; PNG only)
#[tokio::test]
async fn image_fields_scale_down_and_the_related_size_follows_its_source() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "pic").await;

    // smaller than the 1920 bounds: kept at its size; the 128 size is derived from it
    record
        .set_value(&mut tx, "image_1920", b64(&png(400, 200)))
        .await
        .unwrap();
    assert_eq!(size(&mut tx, &record, "image_1920").await, Some((400, 200)));
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((128, 64)));

    // larger than the bounds: scaled down with the aspect ratio kept
    record
        .set_value(&mut tx, "image_1920", b64(&png(2400, 600)))
        .await
        .unwrap();
    assert_eq!(
        size(&mut tx, &record, "image_1920").await,
        Some((1920, 480))
    );
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((128, 32)));

    // smaller than every bound: never enlarged
    record
        .set_value(&mut tx, "image_1920", b64(&png(40, 30)))
        .await
        .unwrap();
    assert_eq!(size(&mut tx, &record, "image_1920").await, Some((40, 30)));
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((40, 30)));

    // an unbounded image field stores the uploaded bytes as they came
    let upload = png(300, 100);
    record
        .set_value(&mut tx, "original", b64(&upload))
        .await
        .unwrap();
    assert_eq!(content(&mut tx, &record, "original").await, Some(upload));

    // the sizes are in the database after the flush, and survive the transaction
    let id = record.ids()[0];
    tx.commit().await.unwrap();
    let mut tx = h.begin().await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([id]).unwrap();
    assert_eq!(size(&mut tx, &record, "image_1920").await, Some((40, 30)));
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((40, 30)));

    // clearing the source clears its stored size
    record
        .set_value(&mut tx, "image_1920", Value::False)
        .await
        .unwrap();
    assert_eq!(content(&mut tx, &record, "image_1920").await, None);
    assert_eq!(content(&mut tx, &record, "image_128").await, None);
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn create_resizes_like_write() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let values = std::collections::BTreeMap::from([
        ("name".to_owned(), Value::str("made")),
        ("image_1920".to_owned(), b64(&png(2000, 1000))),
    ]);
    let record = tx
        .env()
        .records(MODEL)
        .unwrap()
        .create_values(&mut tx, &[values])
        .await
        .unwrap();
    assert_eq!(
        size(&mut tx, &record, "image_1920").await,
        Some((1920, 960))
    );
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((128, 64)));
    drop(tx);
    h.cleanup().await.unwrap();
}

/// pixel limit, are user errors that name the field and the limit; the stored value stays)
#[tokio::test]
async fn non_images_and_oversized_pictures_are_refused() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "pic").await;
    record
        .set_value(&mut tx, "image_1920", b64(&png(64, 64)))
        .await
        .unwrap();

    let error = record
        .set_value(&mut tx, "image_1920", b64(b"just some text, not a picture"))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.image_1920: the file is not a PNG, JPEG, GIF or \
         WebP image"
    );

    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>";
    let error = record
        .set_value(&mut tx, "original", b64(svg))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.original: SVG and other markup is not accepted; \
         use PNG, JPEG, GIF or WebP"
    );

    // a header that declares 20000 x 20000 pixels: refused on the header, nothing is decoded
    let mut huge = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    huge.extend_from_slice(&20_000u32.to_be_bytes());
    huge.extend_from_slice(&20_000u32.to_be_bytes());
    huge.extend_from_slice(&[8, 2, 0, 0, 0]);
    let error = record
        .set_value(&mut tx, "image_1920", b64(&huge))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.image_1920: 20000 x 20000 pixels is above the \
         limit of 40000000 pixels (base.image_max_pixels)"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// stored value outside its range is refused by name)
#[tokio::test]
async fn the_pixel_limit_is_a_bounded_system_parameter() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_max_pixels(&mut tx, "1000000").await;
    let record = create(&mut tx, "pic").await;
    // 1200 x 1000 = 1,200,000 pixels: under the default, above the configured limit
    let error = record
        .set_value(&mut tx, "image_1920", b64(&png(1200, 1000)))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.image_1920: 1200 x 1000 pixels is above the limit \
         of 1000000 pixels (base.image_max_pixels)"
    );
    drop(tx);

    // a stored value below the floor is refused by name, never used
    let mut tx = h.begin().await.unwrap();
    set_max_pixels(&mut tx, "10").await;
    let record = create(&mut tx, "pic").await;
    let error = record
        .set_value(&mut tx, "image_1920", b64(&png(20, 20)))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "base.image_max_pixels is outside the allowed range 1000000..=100000000: 10"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

fn gif(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(width, height, |x, _| {
        image::Rgba([if x % 2 == 0 { 200 } else { 20 }, 30, 60, 255])
    });
    let mut out = Vec::new();
    image::codecs::gif::GifEncoder::new(&mut out)
        .encode_frame(image::Frame::new(picture))
        .unwrap();
    out
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    let picture = image::RgbaImage::from_fn(width, height, |x, y| {
        image::Rgba([(x % 256) as u8, (y % 256) as u8, 10, 255])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(picture)
        .write_with_encoder(image::codecs::webp::WebPEncoder::new_lossless(&mut out))
        .unwrap();
    out
}

/// Format and size of a stored picture, from our own probe, whatever its format.
async fn stored(
    tx: &mut Transaction,
    record: &Records,
    field: &str,
) -> (erp_orm::image::ImageFormat, u32, u32) {
    let bytes = content(tx, record, field).await.unwrap();
    let info = erp_orm::image::probe(&bytes).unwrap();
    (info.format, info.width, info.height)
}

/// smaller than the picture holds a still PNG of the bounds)
#[tokio::test]
async fn gif_and_webp_are_kept_when_they_fit_and_scaled_to_png_when_they_do_not() {
    use erp_orm::image::ImageFormat;
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "pic").await;
    for (upload, format) in [
        (gif(300, 150), ImageFormat::Gif),
        (webp(300, 150), ImageFormat::WebP),
    ] {
        record
            .set_value(&mut tx, "image_1920", b64(&upload))
            .await
            .unwrap();
        assert_eq!(
            content(&mut tx, &record, "image_1920").await,
            Some(upload.clone())
        );
        assert_eq!(
            stored(&mut tx, &record, "image_1920").await,
            (format, 300, 150)
        );
        assert_eq!(
            stored(&mut tx, &record, "image_128").await,
            (ImageFormat::Png, 128, 64)
        );
    }
    // the same through the database, in a new transaction
    let id = record.ids()[0];
    tx.commit().await.unwrap();
    let mut tx = h.begin().await.unwrap();
    let record = tx.env().records(MODEL).unwrap().browse([id]).unwrap();
    assert_eq!(
        stored(&mut tx, &record, "image_128").await,
        (ImageFormat::Png, 128, 64)
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn the_byte_limit_is_a_system_parameter() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_parameter(&mut tx, erp_orm::image::IMAGE_MAX_BYTES_KEY, "65536").await;
    let record = create(&mut tx, "pic").await;
    let mut upload = gif(10, 10);
    upload.resize(70_000, 0);
    let error = record
        .set_value(&mut tx, "original", b64(&upload))
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.original: 70000 bytes is above the limit of 65536 \
         bytes (base.image_max_bytes)"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// the bounds of its target, as create does)
#[tokio::test]
async fn a_write_through_an_editable_related_image_keeps_the_target_bounds() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let record = create(&mut tx, "pic").await;
    record
        .set_value(&mut tx, "photo", b64(&png(2400, 600)))
        .await
        .unwrap();
    assert_eq!(
        size(&mut tx, &record, "image_1920").await,
        Some((1920, 480))
    );
    assert_eq!(size(&mut tx, &record, "image_128").await, Some((128, 32)));
    drop(tx);
    h.cleanup().await.unwrap();
}

#[tokio::test]
async fn a_new_record_holds_the_bounded_picture() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let model = tx.env().records(MODEL).unwrap();
    let vals = RawVals::new().set_cell("image_1920", Cell::Bytes(png(2400, 600).into()));
    let draft = tx.new_record(&model, vals, None, None).await.unwrap();
    assert_eq!(size(&mut tx, &draft, "image_1920").await, Some((1920, 480)));
    drop(tx);
    h.cleanup().await.unwrap();
}

/// declared defaults)
#[tokio::test]
async fn onchange_applies_the_stored_pixel_limit() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    set_max_pixels(&mut tx, "1000000").await;
    let mut form = erp_sdk::ValueMap::new();
    form.insert("image_1920".into(), b64(&png(1200, 1000)));
    let error = tx
        .env()
        .records(MODEL)
        .unwrap()
        .onchange(
            &mut tx,
            form,
            vec!["image_1920".into()],
            erp_orm::Spec::new().field("image_1920").field("image_128"),
        )
        .await
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Validation);
    assert_eq!(
        error.message(),
        "Invalid image for test_image.picture.image_1920: 1200 x 1000 pixels is above the limit \
         of 1000000 pixels (base.image_max_pixels)"
    );
    drop(tx);
    h.cleanup().await.unwrap();
}

/// image limits; the company form's logo is related to its partner's picture)
#[tokio::test]
async fn onchange_through_a_related_picture_loads_the_limits() {
    let h = harness().await;
    let mut tx = h.begin().await.unwrap();
    let model = tx.env().records(MODEL).unwrap();
    for value in [b64(&png(2400, 600)), erp_sdk::Value::False] {
        let mut form = erp_sdk::ValueMap::new();
        form.insert("photo".into(), value);
        model
            .onchange(
                &mut tx,
                form,
                vec!["photo".into()],
                erp_orm::Spec::new().field("photo").field("image_128"),
            )
            .await
            .unwrap();
    }
    drop(tx);
    h.cleanup().await.unwrap();
}
