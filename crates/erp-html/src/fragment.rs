// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded parsing without a cleaning or trust policy.

use crate::budget::HtmlBudget;
use crate::dom::{Dom, NodeId, NodeKind};
use crate::parse::{admit_len, admit_nul, parse_fragment_bounded};
use crate::{HtmlSanitizeError, Limits};

/// An immutable, bounded HTML fragment, **not** sanitized or trusted markup.
///
/// Unknown elements and attributes are retained for consumers to apply their own policy.
/// No mutable parser handles or html5ever types cross this boundary.
#[derive(Debug)]
pub struct Fragment {
    dom: Dom,
}

impl Fragment {
    /// Parse a fragment in an HTML `body` context, without cleaning it.
    ///
    /// Ordinary malformed HTML is recovered by the HTML parser and is not a fatal error.
    /// Input bytes, parser nodes, depth, working bytes and work units remain bounded by
    /// `limits`. Charged nodes include the context element and the generated HTML root;
    /// the document container is retained but not charged as a parser node, matching the
    /// existing parser. The generated root counts as element depth 1. Output and removal-report
    /// limits do not apply because this API neither serializes nor cleans.
    ///
    /// # Examples
    ///
    /// ```
    /// use erp_html::{Fragment, FragmentKind, Limits};
    /// let fragment = Fragment::parse("<p>Hello &amp; welcome</p>", &Limits::ceiling())?;
    /// assert_eq!(fragment.root().kind(), FragmentKind::Container);
    /// # Ok::<(), erp_html::HtmlSanitizeError>(())
    /// ```
    ///
    /// # Errors
    /// Returns the original [`HtmlSanitizeError`] for admission, resource refusal or an
    /// internal inconsistency. A refused parse never returns a partial tree.
    pub fn parse(input: &str, limits: &Limits) -> Result<Self, HtmlSanitizeError> {
        admit_len(input.len(), limits)?;
        admit_nul(input)?;
        let budget = HtmlBudget::new(limits);
        let dom = parse_fragment_bounded(input, &budget)?;
        dom.validate_fragment_links(&budget)?;
        Ok(Self { dom })
    }

    /// The document container. Its children include the generated HTML root.
    #[must_use]
    pub fn root(&self) -> FragmentNode<'_> {
        FragmentNode {
            dom: &self.dom,
            id: self.dom.document(),
        }
    }
}

/// A borrowed, immutable node of a [`Fragment`]. Copying this view allocates nothing.
#[derive(Debug, Clone, Copy)]
pub struct FragmentNode<'a> {
    dom: &'a Dom,
    id: NodeId,
}

/// The content of a fragment node, borrowed from its owning [`Fragment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FragmentKind<'a> {
    /// The document or a detached template-content container.
    Container,
    /// An element; its namespace is the namespace URI, not a prefix.
    Element {
        /// Local element name.
        name: &'a str,
        /// Namespace URI.
        namespace: &'a str,
    },
    /// Decoded character data. Entities have already been decoded once by the parser.
    Text(&'a str),
    /// A comment, doctype or processing instruction, with no exposed text payload.
    Ignored,
}

/// An attribute with its namespace preserved separately from its local name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct FragmentAttribute<'a> {
    /// Local attribute name.
    pub name: &'a str,
    /// Namespace URI; empty for an unqualified attribute.
    pub namespace: &'a str,
    /// Decoded attribute value.
    pub value: &'a str,
}

impl<'a> FragmentNode<'a> {
    /// This node's content. Parsing alone does not establish that any content is safe.
    #[must_use]
    pub fn kind(self) -> FragmentKind<'a> {
        match self.dom.kind(self.id) {
            Some(NodeKind::Document | NodeKind::TemplateContents) => FragmentKind::Container,
            Some(NodeKind::Element { name, .. }) => FragmentKind::Element {
                name: name.local.as_ref(),
                namespace: name.ns.as_ref(),
            },
            Some(NodeKind::Text(text)) => FragmentKind::Text(text),
            Some(NodeKind::Comment(_) | NodeKind::Doctype | NodeKind::ProcessingInstruction) => {
                FragmentKind::Ignored
            }
            // Fragment::parse validates every handle, and no mutation is exposed. Thus
            // this total accessor's fallback cannot be reached by a constructed view.
            None => FragmentKind::Ignored,
        }
    }

    /// Children in document order, without allocating or recursively walking the tree.
    ///
    /// For a template element these are the detached template fragment's children.
    /// Consumers must bound any additional work or storage they build while traversing.
    pub fn children(self) -> impl Iterator<Item = FragmentNode<'a>> + 'a {
        let parent = match self.dom.kind(self.id) {
            Some(NodeKind::Element {
                template_contents: Some(contents),
                ..
            }) => *contents,
            _ => self.id, // Non-template nodes use their ordinary child list.
        };
        self.dom
            .children(parent)
            .iter()
            .copied()
            .map(move |id| Self { dom: self.dom, id })
    }

    /// Attributes in parser order; non-element nodes have none. Values remain untrusted.
    pub fn attributes(self) -> impl Iterator<Item = FragmentAttribute<'a>> + 'a {
        let attrs = match self.dom.kind(self.id) {
            Some(NodeKind::Element { attrs, .. }) => attrs.as_slice(),
            _ => &[], // Only element nodes can carry attributes in the parser DOM.
        };
        attrs.iter().map(|attr| FragmentAttribute {
            name: attr.name.local.as_ref(),
            namespace: attr.name.ns.as_ref(),
            value: attr.value.as_ref(),
        })
    }

    /// A stable structural identifier within this fragment, including generated nodes.
    ///
    /// This is an arena position, not a source byte offset, line number or portable ID.
    #[must_use]
    pub fn index(self) -> usize {
        self.id.index()
    }
}
