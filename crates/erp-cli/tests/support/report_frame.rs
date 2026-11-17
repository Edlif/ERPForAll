// SPDX-License-Identifier: MIT OR Apache-2.0
//! A prepared document as the render worker receives it (a path is tested the way the server
//! runs it). `erp server` prints through `erp report-worker` processes, so every document crosses the
//! worker frame and is admitted again by the worker's builder before layout. The report tests lay out
//! the document that came back, so a node or value the frame cannot carry fails the test the way it
//! fails the print on the server (C33-FIX-PRINT-CELL: the journal entry's Total row did).

use erp_report::{DocTree, Limits};
use erp_report_worker::{Request, WireDocument, decode, encode};

const REFUSED: &str =
    "the document does not cross the render worker frame, so the server cannot print it";

/// `document` after one trip through the frame: encoded as the pool sends it, decoded and admitted
/// under the default limits as the worker does.
pub fn through_frame(document: &DocTree) -> DocTree {
    let frame = WireDocument::from_tree(document).unwrap_or_else(|e| panic!("{REFUSED}: {e}"));
    let sent =
        encode(&Request::Render(frame), usize::MAX).unwrap_or_else(|e| panic!("{REFUSED}: {e}"));
    match decode::<Request<'static>>(&sent).unwrap_or_else(|e| panic!("{REFUSED}: {e}")) {
        Request::Render(frame) => frame
            .into_tree(Limits::default())
            .unwrap_or_else(|e| panic!("{REFUSED}: {e}")),
        other => panic!("a render request decodes as {other:?}"),
    }
}
