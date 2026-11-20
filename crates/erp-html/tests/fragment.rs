// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::expect_used, reason = "test assertions")]

use erp_html::{Fragment, FragmentKind, FragmentNode, HtmlSanitizeError, Limits};

fn html(fragment: &Fragment) -> FragmentNode<'_> {
    let root = fragment.root();
    assert_eq!(root.kind(), FragmentKind::Container);
    root.children().next().expect("generated html root")
}

#[test]
fn preserves_unknown_markup_and_decodes_entities_once() {
    let fragment = Fragment::parse(
        "<custom-box mystery='a&amp;b' onclick='run()'>A&amp;lt;B&nbsp;C</custom-box><!--note-->",
        &Limits::ceiling(),
    )
    .expect("bounded fragment");
    let root = html(&fragment);
    assert_eq!(
        root.kind(),
        FragmentKind::Element {
            name: "html",
            namespace: "http://www.w3.org/1999/xhtml"
        }
    );
    let mut children = root.children();
    let custom = children.next().expect("custom element");
    assert_eq!(
        custom.kind(),
        FragmentKind::Element {
            name: "custom-box",
            namespace: "http://www.w3.org/1999/xhtml"
        }
    );
    let attrs: Vec<_> = custom
        .attributes()
        .map(|a| (a.name, a.namespace, a.value))
        .collect();
    assert_eq!(attrs, [("mystery", "", "a&b"), ("onclick", "", "run()")]);
    let text = custom.children().next().expect("text");
    assert_eq!(text.kind(), FragmentKind::Text("A&lt;B\u{a0}C"));
    assert_eq!(text.attributes().count(), 0);
    assert_eq!(
        children.next().expect("comment").kind(),
        FragmentKind::Ignored
    );
    assert!(children.next().is_none());
    assert_ne!(root.index(), custom.index());
    assert_ne!(custom.index(), text.index());
    assert_eq!(
        custom.index(),
        html(&fragment)
            .children()
            .next()
            .expect("same element")
            .index()
    );
}

#[test]
fn retains_foreign_attribute_namespaces() {
    let fragment = Fragment::parse(
        "<svg><a href='plain' xlink:href='namespaced'>x</a></svg>",
        &Limits::ceiling(),
    )
    .expect("svg");
    let svg = html(&fragment).children().next().expect("svg");
    assert_eq!(
        svg.kind(),
        FragmentKind::Element {
            name: "svg",
            namespace: "http://www.w3.org/2000/svg"
        }
    );
    let link = svg.children().next().expect("a");
    let attrs: Vec<_> = link
        .attributes()
        .map(|a| (a.name, a.namespace, a.value))
        .collect();
    assert_eq!(
        attrs,
        [
            ("href", "", "plain"),
            ("href", "http://www.w3.org/1999/xlink", "namespaced")
        ]
    );
}

#[test]
fn repairs_html_and_exposes_template_contents() {
    let fragment = Fragment::parse(
        "<ul><li>one<li>two</ul><template><b>kept</b></template>",
        &Limits::ceiling(),
    )
    .expect("recovery succeeds");
    let mut children = html(&fragment).children();
    let list = children.next().expect("list");
    let text: Vec<_> = list
        .children()
        .map(|li| li.children().next().expect("text").kind())
        .collect();
    assert_eq!(text, [FragmentKind::Text("one"), FragmentKind::Text("two")]);
    let template = children.next().expect("template");
    let bold = template.children().next().expect("template child");
    assert_eq!(
        bold.children().next().expect("text").kind(),
        FragmentKind::Text("kept")
    );
}

#[test]
fn empty_fragment_counts_generated_and_context_nodes() {
    // Existing bounded parser charges the context body and generated html root.
    let fragment = Fragment::parse("", &Limits::ceiling().with_nodes(2).expect("limit"))
        .expect("two charged nodes");
    assert_eq!(html(&fragment).children().count(), 0);
    let err = Fragment::parse("", &Limits::ceiling().with_nodes(1).expect("limit"))
        .expect_err("context counts");
    assert_eq!(err, HtmlSanitizeError::NodeLimit { limit: 1 });
    assert_eq!(err.to_string(), "document exceeds the node limit of 1");
}

#[test]
fn byte_admission_precedes_nul_scanning() {
    Fragment::parse("é", &Limits::ceiling().with_input_bytes(2).expect("limit"))
        .expect("exact bytes");
    let limits = Limits::ceiling().with_input_bytes(1).expect("limit");
    let err = Fragment::parse("é\0", &limits).expect_err("byte bound first");
    assert_eq!(err, HtmlSanitizeError::InputTooLarge { len: 3, limit: 1 });
    assert_eq!(
        err.to_string(),
        "input of 3 bytes exceeds the limit of 1 bytes"
    );
    let err = Fragment::parse("é\0", &Limits::ceiling()).expect_err("NUL");
    assert_eq!(err, HtmlSanitizeError::NulByte { offset: 2 });
    assert_eq!(err.to_string(), "input contains a NUL byte at offset 2");
}

#[test]
fn generated_root_counts_toward_parser_depth() {
    let limits = Limits::ceiling().with_depth(2).expect("limit");
    Fragment::parse("<div>x</div>", &limits).expect("exact depth");
    let err = Fragment::parse("<div><b>x</b></div>", &limits).expect_err("too deep");
    assert_eq!(err, HtmlSanitizeError::DepthExceeded { limit: 2 });
    assert_eq!(
        err.to_string(),
        "element nesting exceeds the depth limit of 2"
    );
}

#[test]
fn work_and_memory_refusals_are_not_recovery() {
    let err = Fragment::parse(
        "text",
        &Limits::ceiling().with_work_units(1).expect("limit"),
    )
    .expect_err("work refused");
    assert_eq!(err, HtmlSanitizeError::WorkLimit { limit: 1 });
    assert_eq!(
        err.to_string(),
        "sanitising exceeds the work limit of 1 units"
    );
    let err = Fragment::parse(
        "text",
        &Limits::ceiling().with_working_bytes(1).expect("limit"),
    )
    .expect_err("memory refused");
    assert_eq!(err, HtmlSanitizeError::MemoryLimit { limit: 1 });
    assert_eq!(
        err.to_string(),
        "sanitising exceeds the working memory limit of 1 bytes"
    );
}
