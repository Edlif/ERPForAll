// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::unwrap_used, clippy::panic, reason = "test assertions")]
use erp_report::{DocumentBuilder, LimitKind, Limits, Node, PageSetup, ReportError, Style};

#[test]
fn limits_refuse_zero_and_raising_ceiling() {
    for value in [0, 33] {
        let error = Limits::default()
            .lowered(LimitKind::Depth, value)
            .unwrap_err();
        assert!(matches!(error, ReportError::InvalidLimit { .. }));
        assert_eq!(
            error.to_string(),
            format!("report limit depth must be in 1..=32; got {value}")
        );
    }
    assert_eq!(
        Limits::default()
            .lowered(LimitKind::Depth, 16)
            .unwrap()
            .get(LimitKind::Depth),
        16
    );
}

#[test]
fn page_geometry_is_validated() {
    for width in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        let error = PageSetup::new(width, 400.0, 20.0).unwrap_err();
        assert!(matches!(error, ReportError::InvalidDocument { .. }));
        assert_eq!(
            error.to_string(),
            "invalid report document: page dimensions must be finite and positive"
        );
    }
    // A body thinner than one point would turn every line into a page.
    for (width, height, margin) in [
        (40.0, 400.0, 20.0),
        (40.0, 400.0, 19.6),
        (400.0, 40.0, 19.6),
        (1e-300, 400.0, 0.0),
        (400.0, 0.5, 0.0),
        (400.0, 400.0, -1.0),
        (400.0, 400.0, f64::NAN),
    ] {
        let error = PageSetup::new(width, height, margin).unwrap_err();
        assert!(matches!(error, ReportError::InvalidDocument { .. }));
        assert_eq!(
            error.to_string(),
            "invalid report document: page margins must be finite, nonnegative and leave a body of at least 1 point each way"
        );
    }
    let page = PageSetup::new(41.0, 3.0, 1.0).unwrap();
    assert_eq!(
        (page.width(), page.height(), page.margins()),
        (41.0, 3.0, [1.0; 4])
    );
    // per side: a body of 1 point each way is the least admitted
    let page = PageSetup::with_margins(100.0, 50.0, [20.0, 5.0, 29.0, 94.0]).unwrap();
    assert_eq!(page.margins(), [20.0, 5.0, 29.0, 94.0]);
    assert!(PageSetup::with_margins(100.0, 50.0, [20.0, 6.0, 29.0, 94.0]).is_err());
    assert!(PageSetup::with_margins(100.0, 50.0, [-1.0, 0.0, 0.0, 0.0]).is_err());
}

#[test]
fn depth_32_admitted_33_refused() {
    let mut builder = DocumentBuilder::new(Limits::default());
    let mut child = builder.add(Node::Text("leaf".into())).unwrap();
    for _ in 1..32 {
        child = builder
            .add(Node::Block {
                children: vec![child],
                style: Style::default(),
            })
            .unwrap();
    }
    let error = builder
        .add(Node::Block {
            children: vec![child],
            style: Style::default(),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ReportError::DocumentLimit {
            kind: LimitKind::Depth,
            ..
        }
    ));
    assert_eq!(error.to_string(), "report limit depth exceeded: 33 > 32");
}

#[test]
fn repeated_children_charge_expanded_work() {
    let mut builder = DocumentBuilder::new(Limits::default().lowered(LimitKind::Nodes, 4).unwrap());
    let text = builder.add(Node::Text("x".into())).unwrap();
    let error = builder
        .add(Node::Block {
            children: vec![text; 4],
            style: Style::default(),
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ReportError::DocumentLimit {
            kind: LimitKind::Nodes,
            ..
        }
    ));
    assert_eq!(error.to_string(), "report limit nodes exceeded: 5 > 4");
}

#[test]
fn handles_from_another_builder_are_refused() {
    let mut a = DocumentBuilder::new(Limits::default());
    let text = a.add(Node::Text("a".into())).unwrap();
    let mut b = DocumentBuilder::new(Limits::default());
    b.add(Node::Text("b".into())).unwrap();
    let error = b
        .add(Node::Block {
            children: vec![text],
            style: Style::default(),
        })
        .unwrap_err();
    assert!(matches!(error, ReportError::InvalidDocument { .. }));
    assert_eq!(
        error.to_string(),
        "invalid report document: node belongs to another document builder"
    );
}

#[test]
fn invalid_table_shape_is_refused() {
    let mut b = DocumentBuilder::new(Limits::default());
    let cell = b.add(Node::Text("cell".into())).unwrap();
    let e = b
        .add(Node::Table {
            columns: 2,
            header: vec![],
            cells: vec![cell],
        })
        .unwrap_err();
    assert!(matches!(e, ReportError::InvalidDocument { .. }));
    assert_eq!(
        e.to_string(),
        "invalid report document: table rows must fill a positive column count no larger than its cells"
    );
}

#[test]
fn lower_engine_limits_count_unused_arena_storage() {
    let mut b = DocumentBuilder::new(Limits::default());
    b.add(Node::Text("unused".repeat(1000))).unwrap();
    let root = b.add(Node::Text("body".into())).unwrap();
    let options = erp_report::DocumentOptions::new(
        "id",
        "title",
        PageSetup::new(400.0, 500.0, 20.0).unwrap(),
    );
    let doc = b.finish(root, options).unwrap();
    let e = doc
        .check_limits(Limits::default().lowered(LimitKind::Bytes, 1000).unwrap())
        .unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Bytes,
            ..
        }
    ));
    // Actual includes target-dependent Rust node storage; the stable error contract still includes it.
    if let ReportError::DocumentLimit { actual, max, .. } = e {
        assert_eq!(
            e.to_string(),
            format!("report limit bytes exceeded: {actual} > {max}")
        );
        assert!(actual >= 6000);
    }
}

#[test]
fn invalid_font_size_refused() {
    let e = Style::default().with_size(f64::NAN).unwrap_err();
    assert!(matches!(e, ReportError::InvalidDocument { .. }));
    assert_eq!(
        e.to_string(),
        "invalid report document: font size must be finite and in (0, 1440] points"
    );
}

#[test]
fn retained_capacities_are_charged() {
    let limits = Limits::default().lowered(LimitKind::Bytes, 1024).unwrap();
    let mut text = String::with_capacity(4096);
    text.push('x');
    let mut b = DocumentBuilder::new(limits);
    let e = b.add(Node::Text(text)).unwrap_err();
    assert_byte_limit(e);
    let child = b.add(Node::Text("x".into())).unwrap();
    let mut columns = Vec::with_capacity(4096);
    columns.push(1.0);
    assert_byte_limit(
        b.add(Node::Grid {
            columns,
            gutter: 0.0,
            cells: vec![child.clone()],
            align: erp_report::CrossAlignment::Start,
        })
        .unwrap_err(),
    );
    let mut title = String::with_capacity(4096);
    title.push('x');
    assert_byte_limit(
        b.finish(
            child,
            erp_report::DocumentOptions::new(
                "id",
                title,
                PageSetup::new(400.0, 500.0, 20.0).unwrap(),
            ),
        )
        .unwrap_err(),
    );
}
fn assert_byte_limit(e: ReportError) {
    let ReportError::DocumentLimit {
        kind: LimitKind::Bytes,
        actual,
        max,
    } = e
    else {
        panic!("wrong error: {e}")
    };
    assert!(actual >= 4096);
    assert_eq!(max, 1024);
    assert_eq!(
        e.to_string(),
        format!("report limit bytes exceeded: {actual} > 1024")
    );
}
#[test]
fn inline_container_styles_are_refused() {
    let mut b = DocumentBuilder::new(Limits::default());
    let child = b.add(Node::Text("x".into())).unwrap();
    let e = b
        .add(Node::Inline {
            children: vec![child],
            style: Style::default().with_padding(1.0).unwrap(),
        })
        .unwrap_err();
    assert!(matches!(
        e,
        ReportError::Unsupported {
            feature: "inline container styling",
            task: "a later release"
        }
    ));
    assert_eq!(
        e.to_string(),
        "report inline container styling requires a later release"
    );
}

#[test]
fn every_limit_has_positive_lowerable_bounds() {
    for kind in [
        LimitKind::Nodes,
        LimitKind::Bytes,
        LimitKind::Depth,
        LimitKind::ImageBytes,
        LimitKind::ImagePixels,
        LimitKind::Pages,
        LimitKind::PdfBytes,
        LimitKind::EvictionInterval,
        LimitKind::FontBytes,
        LimitKind::WorkerThreads,
    ] {
        let default = Limits::default();
        let max = default.get(kind);
        assert_eq!(default.lowered(kind, 1).unwrap().get(kind), 1);
        for value in [0, max + 1] {
            let e = default.lowered(kind, value).unwrap_err();
            assert!(matches!(e, ReportError::InvalidLimit { .. }));
            assert_eq!(
                e.to_string(),
                format!("report limit {kind} must be in 1..={max}; got {value}")
            );
        }
    }
}

#[test]
fn unused_expansion_counts_against_engine_byte_limit() {
    let mut b = DocumentBuilder::new(Limits::default());
    let text = b.add(Node::Text("x".repeat(600))).unwrap();
    b.add(Node::Flow {
        children: vec![text; 10],
    })
    .unwrap();
    let body = b.add(Node::Text("small body".into())).unwrap();
    let doc = b
        .finish(
            body,
            erp_report::DocumentOptions::new(
                "id",
                "title",
                PageSetup::new(400.0, 500.0, 20.0).unwrap(),
            ),
        )
        .unwrap();
    let e = doc
        .check_limits(Limits::default().lowered(LimitKind::Bytes, 4096).unwrap())
        .unwrap_err();
    let ReportError::DocumentLimit {
        kind: LimitKind::Bytes,
        actual,
        max: 4096,
    } = e
    else {
        panic!("{e}")
    };
    assert!(actual >= 6000);
    assert_eq!(
        e.to_string(),
        format!("report limit bytes exceeded: {actual} > 4096")
    );
}

#[test]
fn tracks_without_cells_are_refused() {
    let mut b = DocumentBuilder::new(Limits::default());
    let e = b
        .add(Node::Table {
            columns: 400_000,
            header: vec![],
            cells: vec![],
        })
        .unwrap_err();
    assert!(matches!(e, ReportError::InvalidDocument { .. }));
    assert_eq!(
        e.to_string(),
        "invalid report document: table rows must fill a positive column count no larger than its cells"
    );
    let cell = b.add(Node::Text("cell".into())).unwrap();
    b.add(Node::Table {
        columns: 1,
        header: vec![],
        cells: vec![cell.clone()],
    })
    .unwrap();
    b.add(Node::Table {
        columns: 1,
        header: vec![],
        cells: vec![],
    })
    .unwrap();
    for weight in [1_000_000.5, f64::MAX] {
        let e = b
            .add(Node::Grid {
                columns: vec![weight],
                gutter: 0.0,
                cells: vec![cell.clone()],
                align: erp_report::CrossAlignment::Start,
            })
            .unwrap_err();
        assert!(matches!(e, ReportError::InvalidDocument { .. }));
        assert_eq!(
            e.to_string(),
            "invalid report document: grid requires finite column weights in (0, 1000000] and a gutter in [0, 1440] points"
        );
    }
    b.add(Node::Grid {
        columns: vec![1_000_000.0],
        gutter: 1440.0,
        cells: vec![cell],
        align: erp_report::CrossAlignment::Start,
    })
    .unwrap();
}

#[test]
fn page_breaks_beyond_the_page_limit_are_refused() {
    let limits = Limits::default().lowered(LimitKind::Pages, 3).unwrap();
    let mut b = DocumentBuilder::new(limits);
    let page_break = b.add(Node::PageBreak).unwrap();
    let three_pages = b
        .add(Node::Flow {
            children: vec![page_break.clone(); 2],
        })
        .unwrap();
    let e = b
        .add(Node::Flow {
            children: vec![page_break; 3],
        })
        .unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Pages,
            actual: 4,
            max: 3
        }
    ));
    assert_eq!(e.to_string(), "report limit pages exceeded: 4 > 3");
    // An engine with a lower page limit refuses the admitted document before it lays anything out.
    let options = erp_report::DocumentOptions::new(
        "id",
        "title",
        PageSetup::new(400.0, 500.0, 20.0).unwrap(),
    );
    let doc = b.finish(three_pages, options).unwrap();
    doc.check_limits(limits).unwrap();
    let e = doc
        .check_limits(limits.lowered(LimitKind::Pages, 2).unwrap())
        .unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Pages,
            actual: 3,
            max: 2
        }
    ));
    assert_eq!(e.to_string(), "report limit pages exceeded: 3 > 2");
}

/// carry safely are refused at construction)
#[test]
fn language_and_document_id_are_validated() {
    let page = || PageSetup::new(400.0, 500.0, 20.0).unwrap();
    let options = || erp_report::DocumentOptions::new("id", "title", page());
    for language in ["", "e", "engl", "en-US", "EN", "e1", "---"] {
        let e = options().with_language(language).unwrap_err();
        assert!(matches!(e, ReportError::InvalidDocument { .. }));
        assert_eq!(
            e.to_string(),
            "invalid report document: language must be two or three lowercase ASCII letters"
        );
    }
    for language in ["en", "ar", "fil"] {
        assert_eq!(
            options().with_language(language).unwrap().language(),
            language
        );
    }
    let long = "x".repeat(257);
    for id in [
        "",
        long.as_str(),
        "line\nbreak",
        "sep\u{2028}arator",
        "over\u{202e}ride",
        "caf\u{e9}",
    ] {
        let mut b = DocumentBuilder::new(Limits::default());
        let body = b.add(Node::Text("body".into())).unwrap();
        let e = b
            .finish(body, erp_report::DocumentOptions::new(id, "title", page()))
            .unwrap_err();
        assert!(matches!(e, ReportError::InvalidDocument { .. }));
        assert_eq!(
            e.to_string(),
            "invalid report document: document id must be 1..=256 printable ASCII characters"
        );
    }
    let mut b = DocumentBuilder::new(Limits::default());
    let body = b.add(Node::Text("body".into())).unwrap();
    let id = "sale.order/42 (copy)";
    let doc = b
        .finish(body, erp_report::DocumentOptions::new(id, "title", page()))
        .unwrap();
    assert_eq!(doc.options().id(), id);
}

/// renderer's nesting budget needs; the layout side is tested in the adapter)
#[test]
fn tables_and_running_regions_are_charged_extra_depth() {
    let options = || {
        erp_report::DocumentOptions::new("id", "title", PageSetup::new(400.0, 500.0, 20.0).unwrap())
    };
    let mut b = DocumentBuilder::new(Limits::default());
    let mut table = b.add(Node::Text("cell".into())).unwrap();
    for _ in 0..15 {
        table = b
            .add(Node::Table {
                columns: 1,
                header: vec![],
                cells: vec![table],
            })
            .unwrap();
    }
    let e = b
        .add(Node::Table {
            columns: 1,
            header: vec![table],
            cells: vec![],
        })
        .unwrap_err();
    assert!(matches!(
        e,
        ReportError::DocumentLimit {
            kind: LimitKind::Depth,
            actual: 33,
            max: 32
        }
    ));
    assert_eq!(e.to_string(), "report limit depth exceeded: 33 > 32");

    for (containers, admitted) in [(30, true), (31, false)] {
        let mut b = DocumentBuilder::new(Limits::default());
        let mut region = b.add(Node::Text("running".into())).unwrap();
        for _ in 0..containers {
            region = b
                .add(Node::Block {
                    children: vec![region],
                    style: Style::default(),
                })
                .unwrap();
        }
        // As a body the same tree is within the ceiling either way.
        b.set_footer(region.clone()).unwrap();
        let result = b.finish(region, options());
        if admitted {
            result.unwrap().check_limits(Limits::default()).unwrap();
        } else {
            let e = result.unwrap_err();
            assert!(matches!(
                e,
                ReportError::DocumentLimit {
                    kind: LimitKind::Depth,
                    actual: 33,
                    max: 32
                }
            ));
            assert_eq!(e.to_string(), "report limit depth exceeded: 33 > 32");
        }
    }
}
