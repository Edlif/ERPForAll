// SPDX-License-Identifier: MIT OR Apache-2.0
//! Optional content (layers): kept by rotate and stamp, rebuilt in reading copies.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::single_range_in_vec_init,
    reason = "tests; one-range selections are deliberate cases"
)]
mod support;
use erp_pdf::{
    AnnotationPolicy, PageSelection, PdfError, PdfErrorKind, PdfLimits, ReadingCopy, Rotation,
    StampPlacement, extract, merge, rotate, split, stamp,
};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};
use support::*;

/// Objects of the layered fixture a configuration can name.
struct Ids {
    /// Layer of page 1.
    draft: ObjectId,
    /// Layer of page 2.
    notes: ObjectId,
    /// Listed group that no page uses.
    unused: ObjectId,
    /// An object that is not a group (the font).
    font: ObjectId,
}
impl Ids {
    fn all(&self) -> Vec<Object> {
        vec![self.draft.into(), self.notes.into(), self.unused.into()]
    }
}
fn group(d: &mut Document, name: &str) -> ObjectId {
    d.add_object(dictionary! {"Type"=>"OCG","Name"=>Object::string_literal(name)})
}
/// Three pages with their own text. Page 1 fills a block inside layer "Draft", page 2 inside layer
/// "Notes", page 3 uses no layer. `configure` writes the catalog entry; `None` leaves it out.
fn layered(configure: impl FnOnce(&Ids) -> Option<Dictionary>) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let root = d.new_object_id();
    let font =
        d.add_object(dictionary! {"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica"});
    let ids = Ids {
        draft: group(&mut d, "Draft"),
        notes: group(&mut d, "Notes"),
        unused: group(&mut d, "Unused"),
        font,
    };
    let mut kids = Vec::new();
    for (label, layer) in [
        ("ONE", Some(ids.draft)),
        ("TWO", Some(ids.notes)),
        ("THREE", None),
    ] {
        let mut content = format!("BT /F1 18 Tf 20 80 Td ({label}) Tj ET\n");
        let mut resources = dictionary! {"Font"=>dictionary!{"F1"=>font}};
        if let Some(layer) = layer {
            content.push_str("/OC /L BDC 0 0 0 rg 100 10 80 60 re f EMC\n");
            resources.set("Properties", dictionary! {"L"=>layer});
        }
        let stream = d.add_object(Stream::new(dictionary! {}, content.into_bytes()));
        kids.push(Object::Reference(d.add_object(dictionary! {
            "Type"=>"Page","Parent"=>root,"Contents"=>stream,"Resources"=>resources,
            "MediaBox"=>vec![0.into(),0.into(),200.into(),120.into()],
        })));
    }
    d.objects.insert(
        root,
        Object::Dictionary(dictionary! {"Type"=>"Pages","Kids"=>kids,"Count"=>3}),
    );
    let mut catalog = dictionary! {"Type"=>"Catalog","Pages"=>root};
    if let Some(properties) = configure(&ids) {
        catalog.set("OCProperties", properties);
    }
    let catalog = d.add_object(catalog);
    d.trailer.set("Root", catalog);
    bytes(d)
}
/// The full configuration: "Draft" and "Unused" hidden, every presentation entry, one alternate.
fn full(g: &Ids, hidden: Vec<Object>) -> Dictionary {
    dictionary! {
        "OCGs"=>g.all(),
        "D"=>dictionary!{
            "Name"=>Object::string_literal("Default"),
            "Creator"=>Object::string_literal("Fixture"),
            "ListMode"=>"VisiblePages",
            "OFF"=>hidden,
            "Order"=>vec![
                g.draft.into(),
                Object::Array(vec![Object::string_literal("Sub"), g.notes.into()]),
                g.unused.into(),
            ],
            "AS"=>vec![Object::Dictionary(dictionary!{
                "Event"=>"Print",
                "Category"=>vec![Object::Name(b"Print".to_vec())],
                "OCGs"=>vec![g.draft.into(), g.unused.into()],
            })],
            "RBGroups"=>vec![Object::Array(vec![g.draft.into(), g.notes.into()])],
            "Locked"=>vec![g.draft.into(), g.unused.into()],
        },
        "Configs"=>vec![Object::Dictionary(dictionary!{
            "Name"=>Object::string_literal("Alternate"),"BaseState"=>"OFF",
        })],
    }
}
/// "Draft" hidden by default.
fn hidden() -> Vec<u8> {
    layered(|g| Some(full(g, vec![g.draft.into(), g.unused.into()])))
}
/// The same document with "Draft" shown by default.
fn shown() -> Vec<u8> {
    layered(|g| Some(full(g, vec![g.unused.into()])))
}
/// "Draft" hidden through the base state: everything off except "Notes".
fn base_off() -> Vec<u8> {
    layered(|g| {
        Some(dictionary! {
            "OCGs"=>g.all(),
            "D"=>dictionary!{"BaseState"=>"OFF","ON"=>vec![g.notes.into()]},
        })
    })
}
fn extracted(input: &[u8], pages: Vec<u32>) -> Result<Vec<u8>, PdfError> {
    let l = PdfLimits::default();
    let selection = PageSelection::new(pages, &l).unwrap();
    collect(|emit| extract(input, &selection, &l, emit))
}
fn merged(inputs: Vec<Vec<u8>>) -> Result<Vec<u8>, PdfError> {
    collect(|emit| merge(inputs, &PdfLimits::default(), emit))
}
fn rotated(input: &[u8]) -> Vec<u8> {
    let l = PdfLimits::default();
    let selection = PageSelection::new(vec![1], &l).unwrap();
    collect(|emit| rotate(input, &selection, Rotation::new(90).unwrap(), &l, emit)).unwrap()
}
fn stamped(input: &[u8], overlay: &[u8]) -> Result<Vec<u8>, PdfError> {
    let l = PdfLimits::default();
    let selection = PageSelection::new(vec![1], &l).unwrap();
    let placement = StampPlacement::new([1., 0., 0., 1., 0., 0.]).unwrap();
    collect(|emit| stamp(input, overlay, &selection, &placement, &l, emit))
}
fn refused(result: Result<Vec<u8>, PdfError>, kind: PdfErrorKind, message: &str) {
    let e = result.expect_err(message);
    assert_eq!(e.to_string(), message);
    assert_eq!(e.kind(), kind, "{message}");
}
fn resolved<'a>(d: &'a Document, mut o: &'a Object) -> &'a Object {
    while let Object::Reference(id) = o {
        o = d.get_object(*id).unwrap();
    }
    o
}
/// A value as text, with every group reference replaced by the group's name, so configurations
/// of documents with different object identifiers compare.
fn shape(d: &Document, o: &Object) -> String {
    match o {
        Object::Reference(_) => match resolved(d, o) {
            Object::Dictionary(g) if g.has(b"Name") => text(g.get(b"Name").unwrap()),
            other => shape(d, other),
        },
        Object::String(..) => format!("({})", text(o)),
        Object::Name(n) => format!("/{}", String::from_utf8_lossy(n)),
        Object::Array(a) => {
            let items: Vec<_> = a.iter().map(|v| shape(d, v)).collect();
            format!("[{}]", items.join(" "))
        }
        Object::Dictionary(v) => {
            let mut items: Vec<_> = v
                .iter()
                .map(|(k, v)| format!("/{} {}", String::from_utf8_lossy(k), shape(d, v)))
                .collect();
            items.sort();
            format!("<<{}>>", items.join(" "))
        }
        other => format!("{other:?}"),
    }
}
fn text(o: &Object) -> String {
    String::from_utf8(o.as_str().unwrap().to_vec()).unwrap()
}
/// The optional content entry of the catalog, as text; `None` when the catalog has none.
fn configuration(d: &Document) -> Option<String> {
    d.catalog()
        .unwrap()
        .get(b"OCProperties")
        .ok()
        .map(|o| shape(d, resolved(d, o)))
}
/// The object the content of a page names as its layer.
fn layer_of(d: &Document, page: u32) -> ObjectId {
    let page = d.get_dictionary(d.get_pages()[&page]).unwrap();
    let resources = resolved(d, page.get(b"Resources").unwrap())
        .as_dict()
        .unwrap();
    resolved(d, resources.get(b"Properties").unwrap())
        .as_dict()
        .unwrap()
        .get(b"L")
        .unwrap()
        .as_reference()
        .unwrap()
}
/// Identifiers the catalog lists under a key of the optional content entry or of its `D`.
fn listed(d: &Document, key: &[u8]) -> Vec<ObjectId> {
    let properties = resolved(d, d.catalog().unwrap().get(b"OCProperties").unwrap())
        .as_dict()
        .unwrap();
    let holder = if key == b"OCGs" {
        properties
    } else {
        resolved(d, properties.get(b"D").unwrap())
            .as_dict()
            .unwrap()
    };
    holder.get(key).map_or_else(
        |_| Vec::new(),
        |v| {
            resolved(d, v)
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_reference().unwrap())
                .collect()
        },
    )
}
fn group_count(d: &Document) -> usize {
    d.objects
        .values()
        .filter(|o| {
            o.as_dict()
                .is_ok_and(|v| matches!(v.get(b"Type").and_then(Object::as_name), Ok(b"OCG")))
        })
        .count()
}

#[test]
fn reading_copies_keep_the_default_visibility_of_layers() {
    let (hidden, shown, base_off) = (hidden(), shown(), base_off());
    // The renderer honours the source configuration, so a raster tells hidden from shown.
    let hidden_page = raster_page(&hidden, 0);
    let shown_page = raster_page(&shown, 0);
    assert_ne!(hidden_page, shown_page);
    assert_eq!(raster_page(&base_off, 0), hidden_page);

    let out = extracted(&hidden, vec![1]).unwrap();
    let d = reopen(&out, 1);
    assert_eq!(listed(&d, b"OCGs"), [layer_of(&d, 1)]);
    assert_eq!(listed(&d, b"OFF"), [layer_of(&d, 1)]);
    assert_eq!(raster(&out), hidden_page);
    assert_eq!(out, extracted(&hidden, vec![1]).unwrap());

    let out = extracted(&shown, vec![1]).unwrap();
    let d = reopen(&out, 1);
    assert_eq!(listed(&d, b"OCGs"), [layer_of(&d, 1)]);
    assert!(listed(&d, b"OFF").is_empty());
    assert_eq!(raster(&out), shown_page);

    // A base state of OFF is written out group by group.
    let out = extracted(&base_off, vec![1, 2, 3]).unwrap();
    let d = reopen(&out, 3);
    assert_eq!(
        configuration(&d).unwrap(),
        "<</D <</BaseState /ON /OFF [Draft]>> /OCGs [Draft Notes]>>"
    );
    assert_eq!(listed(&d, b"OFF"), [layer_of(&d, 1)]);
    assert_eq!(raster_page(&out, 0), hidden_page);
    assert_eq!(raster_page(&out, 1), raster_page(&base_off, 1));

    // Merge: inputs with different base states and the same source identifiers stay apart.
    let out = merged(vec![fixture(&["PLAIN"]), hidden.clone(), base_off, shown]).unwrap();
    let d = reopen(&out, 10);
    let groups = listed(&d, b"OCGs");
    assert_eq!(
        groups,
        [2, 3, 5, 6, 8, 9].map(|page| layer_of(&d, page)),
        "each input's groups are listed once, in input order"
    );
    assert_eq!(
        groups
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        6
    );
    assert_eq!(listed(&d, b"OFF"), [layer_of(&d, 2), layer_of(&d, 5)]);
    assert_eq!(raster_page(&out, 1), hidden_page);
    assert_eq!(raster_page(&out, 4), hidden_page);
    assert_eq!(raster_page(&out, 7), shown_page);

    // Split: every result has its own configuration.
    let l = PdfLimits::default();
    let mut parts = Vec::new();
    split(&hidden, &[1..=1, 2..=3], &l, &mut |b| {
        parts.push(b.to_vec());
        Ok(())
    })
    .unwrap();
    let first = reopen(&parts[0], 1);
    assert_eq!(listed(&first, b"OFF"), [layer_of(&first, 1)]);
    assert_eq!(raster(&parts[0]), hidden_page);
    let second = reopen(&parts[1], 2);
    assert_eq!(listed(&second, b"OCGs"), [layer_of(&second, 1)]);
    assert!(listed(&second, b"OFF").is_empty());
    assert_eq!(raster(&parts[1]), raster_page(&hidden, 1));
}

#[test]
fn the_configuration_is_rebuilt_for_the_copied_groups_only() {
    let hidden = hidden();
    let d = reopen(&extracted(&hidden, vec![1, 2]).unwrap(), 2);
    assert_eq!(
        configuration(&d).unwrap(),
        "<</D <</AS [<</Category [/Print] /Event /Print /OCGs [Draft]>>] /BaseState /ON \
         /Locked [Draft] /OFF [Draft] /Order [Draft [(Sub) Notes]] /RBGroups [[Draft Notes]]>> \
         /OCGs [Draft Notes]>>"
    );
    assert_eq!(group_count(&d), 2, "the unused group is not copied");

    // Only page 2: the label stays with its list; entries that keep no group are dropped.
    let d = reopen(&extracted(&hidden, vec![2]).unwrap(), 1);
    assert_eq!(
        configuration(&d).unwrap(),
        "<</D <</BaseState /ON /Order [[(Sub) Notes]] /RBGroups [[Notes]]>> /OCGs [Notes]>>"
    );

    // The children of a group that is not copied move up a level.
    let nested = layered(|g| {
        Some(dictionary! {
            "OCGs"=>g.all(),
            "D"=>dictionary!{"Order"=>vec![
                g.draft.into(),
                Object::Array(vec![g.notes.into()]),
                Object::Array(vec![Object::string_literal("Empty"), g.unused.into()]),
            ]},
        })
    });
    let d = reopen(&extracted(&nested, vec![2]).unwrap(), 1);
    assert_eq!(
        configuration(&d).unwrap(),
        "<</D <</BaseState /ON /Order [Notes]>> /OCGs [Notes]>>"
    );
    let d = reopen(&extracted(&nested, vec![1, 2]).unwrap(), 2);
    assert_eq!(
        configuration(&d).unwrap(),
        "<</D <</BaseState /ON /Order [Draft [Notes]]>> /OCGs [Draft Notes]>>"
    );

    // Pages without a layer: no configuration and no group dictionary.
    let d = reopen(&extracted(&hidden, vec![3]).unwrap(), 1);
    assert_eq!(configuration(&d), None);
    assert_eq!(group_count(&d), 0);
    // A view-only intent is the default and is accepted.
    for intent in [
        Object::Name(b"View".to_vec()),
        Object::Array(vec![Object::Name(b"View".to_vec())]),
    ] {
        let input =
            layered(|g| Some(dictionary! {"OCGs"=>g.all(),"D"=>dictionary!{"Intent"=>intent}}));
        let d = reopen(&extracted(&input, vec![1]).unwrap(), 1);
        assert_eq!(
            configuration(&d).unwrap(),
            "<</D <</BaseState /ON>> /OCGs [Draft]>>"
        );
    }
}

fn page_content(input: &[u8], content: &str) -> Vec<u8> {
    let mut d = Document::load_mem(input).unwrap();
    let page = d.get_pages()[&1];
    let stream = d
        .get_dictionary(page)
        .unwrap()
        .get(b"Contents")
        .unwrap()
        .as_reference()
        .unwrap();
    *d.get_object_mut(stream).unwrap() =
        Object::Stream(Stream::new(dictionary! {}, content.as_bytes().to_vec()));
    bytes(d)
}
fn without_configuration(input: &[u8]) -> Vec<u8> {
    let mut d = Document::load_mem(input).unwrap();
    let root = d.trailer.get(b"Root").unwrap().as_reference().unwrap();
    d.get_dictionary_mut(root).unwrap().remove(b"OCProperties");
    bytes(d)
}

#[test]
fn whole_document_operations_keep_layers_and_stamp_outside_them() {
    let (hidden, shown) = (hidden(), shown());
    let source = Document::load_mem(&hidden).unwrap();
    let expected = configuration(&source).unwrap();
    assert!(expected.contains("(Alternate)") && expected.contains("/ListMode /VisiblePages"));

    let out = rotated(&hidden);
    let d = reopen(&out, 3);
    assert_eq!(configuration(&d).unwrap(), expected);
    assert_eq!(layer_of(&d, 1), layer_of(&source, 1));
    assert_eq!(listed(&d, b"OFF"), listed(&source, b"OFF"));
    assert_ne!(raster_page(&out, 0), raster_page(&rotated(&shown), 0));
    assert_eq!(raster_page(&out, 1), raster_page(&hidden, 1));

    let overlay = fixture(&["STAMP"]);
    let out = stamped(&hidden, &overlay).unwrap();
    let d = reopen(&out, 3);
    assert_eq!(configuration(&d).unwrap(), expected);
    assert_eq!(layer_of(&d, 1), layer_of(&source, 1));
    assert!(form_text(&out).contains("STAMP"));
    let stamped_hidden = raster_page(&out, 0);
    assert_ne!(
        stamped_hidden,
        raster_page(&hidden, 0),
        "the overlay is drawn"
    );
    assert_ne!(
        stamped_hidden,
        raster_page(&stamped(&shown, &overlay).unwrap(), 0),
        "the hidden layer stays hidden under the overlay"
    );

    // A section left open could belong to a hidden layer and would swallow the overlay.
    for content in [
        "/OC /L BDC 0 0 0 rg 100 10 80 60 re f\n",
        "/Tag BMC 0 0 0 rg 100 10 80 60 re f\n",
        "0 0 0 rg 100 10 80 60 re f EMC\n",
    ] {
        refused(
            stamped(&page_content(&hidden, content), &overlay),
            PdfErrorKind::Malformed,
            "PDF unbalanced marked content",
        );
    }
    refused(
        stamped(&hidden, &page_content(&overlay, "/Tag BMC\n")),
        PdfErrorKind::Malformed,
        "PDF unbalanced marked content",
    );
    // Rotation writes no content and keeps such a page as it is.
    reopen(
        &rotated(&page_content(
            &hidden,
            "/OC /L BDC 0 0 0 rg 100 10 80 60 re f\n",
        )),
        3,
    );

    // An overlay with layers: the target catalog would not list its groups.
    let one_page = extracted(&hidden, vec![1]).unwrap();
    for overlay in [one_page.clone(), without_configuration(&one_page)] {
        refused(
            stamped(&fixture(&["TARGET"]), &overlay),
            PdfErrorKind::Unsupported,
            "PDF feature overlay optional content is unsupported (a later release)",
        );
    }
}

#[test]
fn inconsistent_configurations_are_refused_by_reading_copies() {
    type Configure = Box<dyn FnOnce(&Ids) -> Option<Dictionary>>;
    let name = |n: &str| Object::Name(n.as_bytes().to_vec());
    let d = |entries: Dictionary| -> Configure {
        Box::new(move |g: &Ids| Some(dictionary! {"OCGs"=>g.all(),"D"=>entries}))
    };
    let with = |f: fn(&Ids) -> Dictionary| -> Configure { Box::new(move |g: &Ids| Some(f(g))) };
    let cases: Vec<(Configure, PdfErrorKind, &str)> = vec![
        (
            with(|g| dictionary! {"OCGs"=>g.all()}),
            PdfErrorKind::Malformed,
            "PDF optional content lacks a default configuration",
        ),
        (
            with(|_| dictionary! {"D"=>dictionary!{}}),
            PdfErrorKind::Malformed,
            "PDF optional content lacks its group list",
        ),
        (
            with(|g| dictionary! {"OCGs"=>vec![g.font.into()],"D"=>dictionary!{}}),
            PdfErrorKind::Malformed,
            "PDF optional content group list names an object that is not a group",
        ),
        (
            with(|_| {
                dictionary! {
                    "OCGs"=>vec![Object::Dictionary(dictionary!{"Type"=>"OCG"})],
                    "D"=>dictionary!{},
                }
            }),
            PdfErrorKind::Malformed,
            "PDF optional content groups must be indirect objects",
        ),
        (
            with(|g| {
                dictionary! {
                    "OCGs"=>vec![g.draft.into(), g.notes.into()],
                    "D"=>dictionary!{"OFF"=>vec![g.unused.into()]},
                }
            }),
            PdfErrorKind::Malformed,
            "PDF optional content OFF names an object that is not a listed group",
        ),
        (
            with(|g| {
                dictionary! {
                    "OCGs"=>g.all(),
                    "D"=>dictionary!{"ON"=>vec![g.draft.into()],"OFF"=>vec![g.draft.into()]},
                }
            }),
            PdfErrorKind::Malformed,
            "PDF optional content group is both ON and OFF in the default configuration",
        ),
        (
            d(dictionary! {"BaseState"=>"Unchanged"}),
            PdfErrorKind::Unsupported,
            "PDF feature optional content base state Unchanged is refused by design",
        ),
        (
            d(dictionary! {"BaseState"=>"Maybe"}),
            PdfErrorKind::Malformed,
            "PDF optional content BaseState must be ON, OFF or Unchanged",
        ),
        (
            d(dictionary! {"Intent"=>"Design"}),
            PdfErrorKind::Unsupported,
            "PDF feature optional content default intent other than View is refused by design",
        ),
        (
            d(dictionary! {"Intent"=>vec![name("View"), name("Design")]}),
            PdfErrorKind::Unsupported,
            "PDF feature optional content default intent other than View is refused by design",
        ),
        (
            d(dictionary! {"Intent"=>7}),
            PdfErrorKind::Malformed,
            "PDF optional content Intent must be a name or an array of names",
        ),
        (
            with(|g| {
                dictionary! {"OCGs"=>g.all(),"D"=>dictionary!{"Order"=>vec![g.font.into()]}}
            }),
            PdfErrorKind::Malformed,
            "PDF optional content Order names an object that is not a listed group",
        ),
        (
            d(dictionary! {"Order"=>vec![Object::Integer(7)]}),
            PdfErrorKind::Malformed,
            "PDF optional content Order entry must be a group, a list or a list label",
        ),
        (
            with(|g| {
                dictionary! {"OCGs"=>g.all(),"D"=>dictionary!{"AS"=>vec![Object::Dictionary(
                    dictionary!{"Event"=>"Sleep","Category"=>vec![Object::Name(b"View".to_vec())],
                        "OCGs"=>vec![g.draft.into()]},
                )]}}
            }),
            PdfErrorKind::Malformed,
            "PDF optional content AS Event must be View, Print or Export",
        ),
        (
            with(|g| {
                dictionary! {"OCGs"=>g.all(),"D"=>dictionary!{"Locked"=>vec![g.font.into()]}}
            }),
            PdfErrorKind::Malformed,
            "PDF optional content Locked names an object that is not a listed group",
        ),
        (
            with(|g| {
                dictionary! {
                    "OCGs"=>g.all(),
                    "D"=>dictionary!{"RBGroups"=>vec![Object::Array(vec![g.font.into()])]},
                }
            }),
            PdfErrorKind::Malformed,
            "PDF optional content RBGroups names an object that is not a listed group",
        ),
        // The page uses a group that the catalog does not list.
        (
            with(|g| {
                dictionary! {"OCGs"=>vec![g.notes.into(), g.unused.into()],"D"=>dictionary!{}}
            }),
            PdfErrorKind::Malformed,
            "PDF optional content group is not listed in the catalog",
        ),
        (
            Box::new(|_| None),
            PdfErrorKind::Malformed,
            "PDF optional content group is not listed in the catalog",
        ),
    ];
    for (configure, kind, message) in cases {
        let input = layered(configure);
        refused(extracted(&input, vec![1]), kind, message);
        refused(merged(vec![fixture(&["PLAIN"]), input]), kind, message);
    }
    // The configuration is checked for the document, also when the selected page uses no layer.
    let input = layered(|g| Some(dictionary! {"OCGs"=>g.all()}));
    refused(
        extracted(&input, vec![3]),
        PdfErrorKind::Malformed,
        "PDF optional content lacks a default configuration",
    );
    // A group the catalog does not list is refused only where a copied page uses it.
    let input = layered(|g| {
        Some(dictionary! {"OCGs"=>vec![g.notes.into(), g.unused.into()],"D"=>dictionary!{}})
    });
    let out = extracted(&input, vec![2, 3]).unwrap();
    assert_eq!(
        configuration(&reopen(&out, 2)).unwrap(),
        "<</D <</BaseState /ON>> /OCGs [Notes]>>"
    );
}

/// One annotation on a page of `input`, with the given optional-content membership.
fn annotated(input: &[u8], page: u32, subtype: &str, membership: Object) -> Vec<u8> {
    let mut d = Document::load_mem(input).unwrap();
    let id = d.get_pages()[&page];
    let mut annotation = dictionary! {
        "Type"=>"Annot",
        "Subtype"=>subtype,
        "Rect"=>vec![10.into(), 10.into(), 60.into(), 30.into()],
        "OC"=>membership,
    };
    if subtype == "Link" {
        annotation.set(
            "A",
            dictionary! {"S"=>"URI","URI"=>Object::string_literal("https://example.test/a")},
        );
    }
    let annotation = d.add_object(annotation);
    d.get_dictionary_mut(id)
        .unwrap()
        .set("Annots", vec![Object::Reference(annotation)]);
    bytes(d)
}
/// Extract pages under an annotation policy: the bytes and the carried and left-out counts.
fn kept(
    input: &[u8],
    pages: Vec<u32>,
    policy: AnnotationPolicy,
) -> Result<(Vec<u8>, usize, usize), PdfError> {
    let l = PdfLimits::default();
    let selection = PageSelection::new(pages, &l).unwrap();
    let mut out = Vec::new();
    let summary =
        ReadingCopy::new()
            .annotations(policy)
            .extract(input, &selection, &l, &mut |b| {
                out.extend_from_slice(b);
                Ok(())
            })?;
    Ok((
        out,
        summary.annotations_kept(),
        summary.annotations_dropped(),
    ))
}
/// The group the first annotation of a page names in `OC`.
fn membership(d: &Document, page: u32) -> ObjectId {
    let page = d.get_dictionary(d.get_pages()[&page]).unwrap();
    let annotations = resolved(d, page.get(b"Annots").unwrap())
        .as_array()
        .unwrap();
    resolved(d, &annotations[0])
        .as_dict()
        .unwrap()
        .get(b"OC")
        .unwrap()
        .as_reference()
        .unwrap()
}

#[test]
fn annotations_of_a_layer_are_carried_with_their_group() {
    let source = hidden();
    let ids = {
        let d = Document::load_mem(&source).unwrap();
        (layer_of(&d, 1), listed(&d, b"OCGs")[2])
    };
    let (draft, unused) = ids;
    // Page 1: a link in the layer its content uses. Page 2: a note in a group no content uses.
    let input = annotated(&source, 1, "Link", draft.into());
    let input = annotated(&input, 2, "Text", unused.into());
    for policy in [
        AnnotationPolicy::KeepSelfContained,
        AnnotationPolicy::KeepOrDrop,
    ] {
        let (out, carried, left_out) = kept(&input, vec![1, 2], policy).unwrap();
        assert_eq!((carried, left_out), (2, 0));
        let o = reopen(&out, 2);
        // The annotation names the same group object as the page content, and the catalog
        // lists every group an annotation uses, hidden ones still hidden.
        assert_eq!(membership(&o, 1), layer_of(&o, 1));
        let groups = listed(&o, b"OCGs");
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0], layer_of(&o, 1));
        assert_eq!(groups[1], layer_of(&o, 2));
        assert_eq!(groups[2], membership(&o, 2));
        assert_eq!(listed(&o, b"OFF"), vec![groups[0], groups[2]]);
        assert_eq!(group_count(&o), 3);
        let (again, ..) = kept(&input, vec![1, 2], policy).unwrap();
        assert_eq!(out, again);
    }
    // Without the annotated page the group only the note uses is not copied.
    let (out, carried, _) = kept(&input, vec![1], AnnotationPolicy::KeepSelfContained).unwrap();
    assert_eq!(carried, 1);
    let o = reopen(&out, 1);
    assert_eq!(listed(&o, b"OCGs"), vec![layer_of(&o, 1)]);
    assert_eq!(group_count(&o), 1);
    // The default policy still refuses the document before any group is looked at.
    refused(
        extracted(&input, vec![1]),
        PdfErrorKind::Unsupported,
        "PDF page annotations are refused by the reading-copy annotation policy",
    );

    // A group the catalog does not list, or one written in place, is never carried.
    let unlisted = layered(|g| {
        Some(dictionary! {"OCGs"=>vec![g.notes.into(), g.unused.into()],"D"=>dictionary!{}})
    });
    let unlisted = annotated(&unlisted, 2, "Link", draft.into());
    let direct = annotated(
        &source,
        2,
        "Link",
        Object::Dictionary(dictionary! {"Type"=>"OCG","Name"=>Object::string_literal("Inline")}),
    );
    for policy in [
        AnnotationPolicy::KeepSelfContained,
        AnnotationPolicy::KeepOrDrop,
    ] {
        refused(
            kept(&unlisted, vec![2], policy).map(|r| r.0),
            PdfErrorKind::Malformed,
            "PDF optional content group is not listed in the catalog",
        );
        refused(
            kept(&direct, vec![2], policy).map(|r| r.0),
            PdfErrorKind::Malformed,
            "PDF optional content groups must be indirect objects",
        );
    }
}
