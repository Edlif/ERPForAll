// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::single_range_in_vec_init,
    reason = "tests; one-range selections are deliberate cases"
)]
mod support;
use erp_pdf::{PageSelection, PdfErrorKind, PdfLimits, Rotation, extract, merge, rotate, split};
use support::*;

#[test]
fn merges_three_documents_in_order() {
    let inputs = [
        fixture(&["FIRST"]),
        fixture(&["SECOND"]),
        fixture(&["THIRD"]),
    ];
    let mut out = Vec::new();
    merge(inputs, &PdfLimits::default(), &mut |b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    let d = reopen(&out, 3);
    for (i, word) in ["FIRST", "SECOND", "THIRD"].iter().enumerate() {
        assert!(d.extract_text(&[i as u32 + 1]).unwrap().contains(word));
    }
    assert!(!raster(&out).is_empty());
}
#[test]
fn extracts_splits_and_rotates_inherited_pages() {
    let input = fixture(&["ONE", "TWO", "THREE"]);
    let limits = PdfLimits::default();
    let selection = PageSelection::new(vec![3, 1], &limits).unwrap();
    let mut out = Vec::new();
    extract(&input, &selection, &limits, &mut |b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    let d = reopen(&out, 2);
    assert!(d.extract_text(&[1]).unwrap().contains("THREE"));
    let mut parts = Vec::new();
    split(&input, &[1..=1, 2..=3], &limits, &mut |b| {
        parts.push(b.to_vec());
        Ok(())
    })
    .unwrap();
    reopen(&parts[0], 1);
    reopen(&parts[1], 2);
    out.clear();
    rotate(
        &input,
        &PageSelection::new(vec![2], &limits).unwrap(),
        Rotation::new(90).unwrap(),
        &limits,
        &mut |b| {
            out.extend_from_slice(b);
            Ok(())
        },
    )
    .unwrap();
    let d = reopen(&out, 3);
    let page = d.get_pages()[&2];
    assert_eq!(
        d.get_object(page)
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
}
#[test]
fn malformed_input_names_ordinal() {
    let e = merge(
        [b"not a pdf".as_slice()],
        &PdfLimits::default(),
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Malformed);
    assert_eq!(e.to_string(), "PDF input 1: malformed document");
}
#[test]
fn empty_and_later_malformed_inputs_stop_delivery() {
    let l = PdfLimits::default();
    let e = merge(Vec::<Vec<u8>>::new(), &l, &mut |_| Ok(())).unwrap_err();
    assert_eq!(e.kind(), erp_pdf::PdfErrorKind::InvalidOptions);
    assert_eq!(e.to_string(), "PDF merge requires at least one document");
    let mut seen = 0;
    let mut chunks = 0;
    let inputs = (0..3).map(|n| {
        seen += 1;
        if n == 1 {
            b"invalid".to_vec()
        } else {
            fixture(&["A"])
        }
    });
    let e = merge(inputs, &l, &mut |_| {
        chunks += 1;
        Ok(())
    })
    .unwrap_err();
    assert_eq!(seen, 2);
    assert!(chunks > 0);
    assert_eq!(e.kind(), erp_pdf::PdfErrorKind::Malformed);
    assert_eq!(e.to_string(), "PDF input 2: malformed document");
}
#[test]
fn invalid_split_ranges_emit_nothing() {
    let l = PdfLimits::default();
    let input = fixture(&["A", "B"]);
    for ranges in [
        vec![],
        vec![1..=1],
        vec![1..=3],
        vec![2..=2],
        vec![1..=1, 1..=2],
        vec![std::ops::RangeInclusive::new(2, 1)],
    ] {
        let mut chunks = 0;
        let e = split(&input, &ranges, &l, &mut |_| {
            chunks += 1;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(chunks, 0);
        assert_eq!(e.kind(), erp_pdf::PdfErrorKind::InvalidOptions);
        assert_eq!(
            e.to_string(),
            "PDF split ranges must partition all pages consecutively"
        );
    }
}
