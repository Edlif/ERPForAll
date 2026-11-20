// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::ReportError;
use erp_views::{LimitKind, ViewLimits};
use std::time::Duration;
/// Caller-selected limits for this database-free admission library. The integration layer owns
/// settings persistence; these ceilings cannot be raised by an addon.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ReportLimits {
    pub(crate) views: ViewLimits,
    pub(crate) html_bytes: usize,
    pub(crate) html_nodes: usize,
    pub(crate) diagnostics: usize,
    pub(crate) calls: usize,
    pub(crate) program_nodes: usize,
    pub(crate) program_depth: usize,
    pub(crate) steps: usize,
    pub(crate) fragments: usize,
    pub(crate) plan_paths: usize,
    pub(crate) data_records: usize,
    pub(crate) bytes: usize,
    pub(crate) deadline: Duration,
}
/// Twice the document's 64 MiB: the text an evaluation prints plus the names its calls and
/// fragments copy.
const EVALUATION_BYTES: usize = 128 * 1024 * 1024;
/// The synchronous per-document deadline
const EVALUATION_DEADLINE: Duration = Duration::from_secs(30);
impl Default for ReportLimits {
    fn default() -> Self {
        Self {
            views: ViewLimits::default(),
            html_bytes: 4 * 1024 * 1024,
            html_nodes: 50_000,
            diagnostics: 256,
            calls: 32,
            program_nodes: 50_000,
            program_depth: 128,
            steps: 10_000_000,
            fragments: 65_536,
            plan_paths: 4_096,
            data_records: 200_000,
            bytes: EVALUATION_BYTES,
            deadline: EVALUATION_DEADLINE,
        }
    }
}
impl ReportLimits {
    /// Per-value HTML admission. Positive host-selected bounds, at most 4 MiB and 50,000 parser nodes.
    pub fn with_html_limits(mut self, bytes: usize, nodes: usize) -> Result<Self, ReportError> {
        for (name, value, maximum) in [
            ("html_bytes", bytes, self.html_bytes),
            ("html_nodes", nodes, self.html_nodes),
        ] {
            if value == 0 || value > maximum {
                return Err(ReportError::Limit { name, maximum });
            }
        }
        self.html_bytes = bytes;
        self.html_nodes = nodes;
        Ok(self)
    }

    /// Use the view engine's independently validated, lower-only limits.
    pub fn with_views(mut self, views: ViewLimits) -> Self {
        self.views = views;
        self
    }
    /// Bound both diagnostics and deferred dynamic-value obligations; floor 1, ceiling 256.
    pub fn with_diagnostics(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=256).contains(&value) {
            return Err(ReportError::Limit {
                name: "diagnostics",
                maximum: 256,
            });
        }
        self.diagnostics = value;
        Ok(self)
    }
    /// Bound static template-call nesting; floor 1, ceiling 32.
    pub fn with_call_depth(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=32).contains(&value) {
            return Err(ReportError::Limit {
                name: "call_depth",
                maximum: 32,
            });
        }
        self.calls = value;
        Ok(self)
    }
    /// Bound the operations of one compiled program, called templates included; floor 1, ceiling
    /// 50,000 (design §6.5).
    pub fn with_program_nodes(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=50_000).contains(&value) {
            return Err(ReportError::Limit {
                name: "program_nodes",
                maximum: 50_000,
            });
        }
        self.program_nodes = value;
        Ok(self)
    }
    /// Bound element nesting of a program across its template calls; floor 1, ceiling 128. The
    /// compiler recurses once per level and the evaluator at most four times, so this bounds
    /// their stack.
    pub fn with_program_depth(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=128).contains(&value) {
            return Err(ReportError::Limit {
                name: "program_depth",
                maximum: 128,
            });
        }
        self.program_depth = value;
        Ok(self)
    }
    /// Bound the evaluation steps of one document (operations, loop iterations and expressions);
    /// floor 1, ceiling 10,000,000 (design §6.5). The data stage separately shares this budget
    /// across helper dependency expansion and accepted results. Each expression also runs.
    pub fn with_evaluation_steps(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=10_000_000).contains(&value) {
            return Err(ReportError::Limit {
                name: "evaluation_steps",
                maximum: 10_000_000,
            });
        }
        self.steps = value;
        Ok(self)
    }
    /// Bound the `t-set` bodies one evaluation may hold (each keeps a copy of the names of its
    /// place); floor 1, ceiling 65,536.
    pub fn with_template_fragments(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=65_536).contains(&value) {
            return Err(ReportError::Limit {
                name: "template_fragments",
                maximum: 65_536,
            });
        }
        self.fragments = value;
        Ok(self)
    }
    /// Bound the relation paths of one prefetch plan (`docs.partner_id.country_id` is three); floor
    /// 1, ceiling 4,096.
    pub fn with_plan_paths(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=4_096).contains(&value) {
            return Err(ReportError::Limit {
                name: "plan_paths",
                maximum: 4_096,
            });
        }
        self.plan_paths = value;
        Ok(self)
    }
    /// Bound the records the data stage loads for one batch, related and named precision rows included; floor 1,
    /// ceiling 200,000.
    pub fn with_data_records(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=200_000).contains(&value) {
            return Err(ReportError::Limit {
                name: "data_records",
                maximum: 200_000,
            });
        }
        self.data_records = value;
        Ok(self)
    }
    /// Bound the bytes one evaluation buffers: the text it prints and the names each `t-call` and
    /// `t-set` body copies; floor 1, ceiling 128 MiB. The data stage separately bounds retained
    /// helper results by logical expression deep size plus record-ID keys under this same ceiling.
    pub fn with_evaluation_bytes(mut self, value: usize) -> Result<Self, ReportError> {
        if !(1..=EVALUATION_BYTES).contains(&value) {
            return Err(ReportError::Limit {
                name: "evaluation_bytes",
                maximum: EVALUATION_BYTES,
            });
        }
        self.bytes = value;
        Ok(self)
    }
    /// Bound the wall-clock time of one evaluation; floor 1 ms, ceiling 30 s (design §6.5). The
    /// error's maximum is in milliseconds.
    pub fn with_evaluation_deadline(mut self, value: Duration) -> Result<Self, ReportError> {
        if !(Duration::from_millis(1)..=EVALUATION_DEADLINE).contains(&value) {
            return Err(ReportError::Limit {
                name: "evaluation_deadline",
                maximum: millis(EVALUATION_DEADLINE),
            });
        }
        self.deadline = value;
        Ok(self)
    }
    pub(crate) fn work(self) -> Work {
        Work {
            left: self.views.get(LimitKind::EvaluationSteps),
            maximum: self.views.get(LimitKind::EvaluationSteps),
        }
    }
}
pub(crate) fn millis(value: Duration) -> usize {
    usize::try_from(value.as_millis()).unwrap_or(usize::MAX)
}
#[derive(Debug)]
pub(crate) struct Work {
    left: usize,
    maximum: usize,
}
impl Work {
    pub(crate) fn charge(&mut self, amount: usize) -> Result<(), ReportError> {
        self.left = self.left.checked_sub(amount).ok_or(ReportError::Limit {
            name: "validation_work",
            maximum: self.maximum,
        })?;
        Ok(())
    }
}
