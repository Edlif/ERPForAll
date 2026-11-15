// SPDX-License-Identifier: MIT OR Apache-2.0
use erp_report::{ReportLimits, validate_arch};
use erp_views::{ViewCatalogue, ViewDescriptor};

#[test]
fn primary_copy_preserves_parent_identity_and_receives_extensions() {
    let catalogue = ViewCatalogue::new(vec![
        ViewDescriptor::new(1, "a.report", "", "<t t-name='a.report'><p>A</p></t>"),
        ViewDescriptor::new(2, "b.report", "", "<data><xpath expr='.' position='attributes'><attribute name='t-name'>b.report</attribute></xpath></data>").primary_of(1),
        ViewDescriptor::new(3, "b.extension", "", "<xpath expr='//p' position='after'><p>B</p></xpath>").extension_of(1),
    ]).unwrap();
    let parent = catalogue.combine(1).unwrap();
    let child = catalogue.combine(2).unwrap();
    assert_eq!(parent.arch().root().attr("t-name"), Some("a.report"));
    assert_eq!(child.arch().root().attr("t-name"), Some("b.report"));
    assert_eq!(
        parent
            .arch()
            .root()
            .children()
            .map(|n| n.text())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    assert_eq!(
        child
            .arch()
            .root()
            .children()
            .map(|n| n.text())
            .collect::<Vec<_>>(),
        ["A", "B"]
    );
    validate_arch("b.report", child.arch(), ReportLimits::default()).unwrap();
}
