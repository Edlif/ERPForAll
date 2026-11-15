// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]
mod support;
use erp_pdf::{Limit, PageSelection, PdfErrorKind, PdfLimits, Rotation, merge};
#[test]
fn zero_limit_is_not_unlimited() {
    let e = PdfLimits::default().with(Limit::InputBytes, 0).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
    assert_eq!(
        e.to_string(),
        "PDF limit input_bytes accepts 1 to 268435456, not 0"
    );
}
#[test]
fn invalid_selection_and_rotation_are_rejected() {
    let l = PdfLimits::default();
    for (pages, msg) in [
        (vec![], "PDF page selection is empty"),
        (vec![0], "PDF page numbers start at 1"),
        (vec![1, 1], "PDF page selection repeats page 1"),
    ] {
        let e = PageSelection::new(pages, &l).unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
        assert_eq!(e.to_string(), msg);
    }
    let e = Rotation::new(45).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
    assert_eq!(
        e.to_string(),
        "PDF rotation must be a multiple of 90 degrees; got 45"
    );
}
#[test]
fn sink_failure_does_not_consume_later_input() {
    let mut seen = 0;
    let inputs = (0..3).map(|_| {
        seen += 1;
        support::fixture(&["A"])
    });
    let e = merge(inputs, &PdfLimits::default(), &mut |_| {
        Err(std::io::Error::other("sink closed"))
    })
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Output);
    assert_eq!(e.to_string(), "PDF output consumer failed");
    assert!(seen <= 1);
}
#[test]
fn orphan_objects_count_across_inputs() {
    let mut d = lopdf::Document::load_mem(&support::fixture(&["A"])).unwrap();
    for _ in 0..100 {
        d.add_object(lopdf::Object::Null);
    }
    let input = support::bytes(d);
    let count = lopdf::Document::load_mem(&input).unwrap().objects.len();
    let l = PdfLimits::default()
        .with(Limit::Objects, count + 8)
        .unwrap();
    let e = merge([input.clone(), input], &l, &mut |_| Ok(())).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    // No inherited direct dictionary promotion in this fixture except the root MediaBox.
    assert_eq!(
        e.to_string(),
        format!(
            "PDF limit objects exceeded: {} > {}",
            count * 2 + 1,
            count + 8
        )
    );
}
#[test]
fn work_budget_is_for_the_operation() {
    let input = support::fixture(&["A"]);
    let l = PdfLimits::default().with(Limit::WorkUnits, 200).unwrap();
    merge([input.clone()], &l, &mut |_| Ok(())).unwrap();
    let e = merge(std::iter::repeat_n(input, 10), &l, &mut |_| Ok(())).unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(e.to_string(), "PDF limit work_units exceeded: 201 > 200");
}
#[test]
fn configured_boundaries_are_enforced() {
    use erp_pdf::Limit::*;
    for dim in [
        InputBytes,
        OutputBytes,
        Documents,
        Pages,
        Objects,
        Depth,
        WorkUnits,
        DecodedBytes,
        MetadataBytes,
    ] {
        for n in [0, dim.ceiling() + 1] {
            let e = PdfLimits::default().with(dim, n).unwrap_err();
            assert_eq!(e.kind(), PdfErrorKind::InvalidOptions);
            assert_eq!(
                e.to_string(),
                format!(
                    "PDF limit {} accepts 1 to {}, not {n}",
                    dim.name(),
                    dim.ceiling()
                )
            );
        }
        assert_eq!(
            PdfLimits::default()
                .with(dim, dim.ceiling())
                .unwrap()
                .get(dim),
            dim.ceiling()
        );
        assert_eq!(PdfLimits::default().with(dim, 1).unwrap().get(dim), 1);
    }
    let input = support::fixture(&["A"]);
    let mut out = Vec::new();
    let l = PdfLimits::default().with(InputBytes, input.len()).unwrap();
    merge([&input], &l, &mut |b| {
        out.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    let e = merge(
        [&input],
        &l.with(InputBytes, input.len() - 1).unwrap(),
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(
        e.to_string(),
        format!(
            "PDF limit input_bytes exceeded: {} > {}",
            input.len(),
            input.len() - 1
        )
    );
    let l = PdfLimits::default().with(OutputBytes, out.len()).unwrap();
    merge([&input], &l, &mut |_| Ok(())).unwrap();
    let e = merge(
        [&input],
        &l.with(OutputBytes, out.len() - 1).unwrap(),
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(e.kind(), PdfErrorKind::Limit);
    assert_eq!(
        e.to_string(),
        format!(
            "PDF limit output_bytes exceeded: {} > {}",
            out.len(),
            out.len() - 1
        )
    );
    for dim in [Documents, Pages] {
        let l = PdfLimits::default().with(dim, 2).unwrap();
        merge([&input, &input], &l, &mut |_| Ok(())).unwrap();
        let e = merge([&input, &input], &l.with(dim, 1).unwrap(), &mut |_| Ok(())).unwrap_err();
        assert_eq!(e.kind(), PdfErrorKind::Limit);
        assert_eq!(
            e.to_string(),
            format!("PDF limit {} exceeded: 2 > 1", dim.name())
        );
    }
}
