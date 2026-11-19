// SPDX-License-Identifier: MIT OR Apache-2.0
//! The formula engine (design §7): `+ - * /`, parentheses, decimal numbers,
//! `CODE.label` (another line of the report), `label` (the same line) and `sum_children` (the same
//! label summed over the line's child lines). Exact decimals, checked arithmetic.
use std::str::FromStr;

use erp_sdk::prelude::*;

// Evaluator limits of untrusted data are host constants, below's
// 64 KiB and depth 64 for accounting formulas.
const MAX_FORMULA_BYTES: usize = 4096;
const MAX_DEPTH: usize = 64;
// operators in one chain (`a + b + c ...`): each one nests the evaluation one level deeper
const MAX_CHAIN: usize = 256;

/// A value a formula names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Ref {
    /// The line's code; `None` for the same line.
    pub code: Option<String>,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Term {
    Num(Decimal),
    Ref(Ref),
    SumChildren,
    Neg(Box<Term>),
    Bin(u8, Box<Term>, Box<Term>),
}

/// Why an evaluation has no number.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum EvalError {
    DivisionByZero,
    Overflow,
}

pub(crate) fn parse(text: &str) -> Result<Term> {
    if text.len() > MAX_FORMULA_BYTES {
        return Err(Error::validation(format!(
            "A formula is longer than {MAX_FORMULA_BYTES} bytes."
        )));
    }
    let mut parser = Parser { text, pos: 0 };
    let term = parser.sum(0)?;
    parser.skip();
    if parser.pos < text.len() {
        return Err(parser.error("unexpected text"));
    }
    Ok(term)
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, what: &str) -> Error {
        Error::validation(format!(
            "Formula '{}': {what} at character {}.",
            self.text,
            self.pos + 1
        ))
    }
    fn skip(&mut self) {
        while self.text[self.pos..].starts_with(' ') {
            self.pos += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip();
        self.text.as_bytes().get(self.pos).copied()
    }
    fn chain(&self, length: usize) -> Result<()> {
        if length > MAX_CHAIN {
            return Err(self.error(&format!("more than {MAX_CHAIN} operators in a row")));
        }
        Ok(())
    }
    fn sum(&mut self, depth: usize) -> Result<Term> {
        let mut left = self.product(depth)?;
        let mut chain = 0;
        while let Some(op @ (b'+' | b'-')) = self.peek() {
            self.pos += 1;
            chain += 1;
            self.chain(chain)?;
            left = Term::Bin(op, Box::new(left), Box::new(self.product(depth)?));
        }
        Ok(left)
    }
    fn product(&mut self, depth: usize) -> Result<Term> {
        let mut left = self.unary(depth)?;
        let mut chain = 0;
        while let Some(op @ (b'*' | b'/')) = self.peek() {
            self.pos += 1;
            chain += 1;
            self.chain(chain)?;
            left = Term::Bin(op, Box::new(left), Box::new(self.unary(depth)?));
        }
        Ok(left)
    }
    fn unary(&mut self, depth: usize) -> Result<Term> {
        if depth >= MAX_DEPTH {
            return Err(self.error(&format!("nesting deeper than {MAX_DEPTH}")));
        }
        match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                Ok(Term::Neg(Box::new(self.unary(depth + 1)?)))
            }
            Some(b'(') => {
                self.pos += 1;
                let inner = self.sum(depth + 1)?;
                if self.peek() != Some(b')') {
                    return Err(self.error("a closing parenthesis is missing"));
                }
                self.pos += 1;
                Ok(inner)
            }
            Some(b'0'..=b'9') => {
                let start = self.pos;
                while matches!(self.text.as_bytes().get(self.pos), Some(b'0'..=b'9' | b'.')) {
                    self.pos += 1;
                }
                Decimal::from_str(&self.text[start..self.pos])
                    .map(Term::Num)
                    .map_err(|_| self.error("a malformed number"))
            }
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'_') => {
                let start = self.pos;
                while matches!(
                    self.text.as_bytes().get(self.pos),
                    Some(b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'.')
                ) {
                    self.pos += 1;
                }
                let word = &self.text[start..self.pos];
                let label = |text: &str| {
                    text.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                        && text
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                };
                match word.split_once('.') {
                    None if word == "sum_children" => Ok(Term::SumChildren),
                    None if label(word) => Ok(Term::Ref(Ref {
                        code: None,
                        label: word.to_owned(),
                    })),
                    Some((code, lab)) if crate::definition::is_code(code) && label(lab) => {
                        Ok(Term::Ref(Ref {
                            code: Some(code.to_owned()),
                            label: lab.to_owned(),
                        }))
                    }
                    _ => Err(self.error(&format!(
                        "'{word}' is not CODE.label, a label or sum_children"
                    ))),
                }
            }
            _ => Err(self.error("a number, a name or a parenthesis is expected")),
        }
    }
}

impl Term {
    /// Every reference, and whether `sum_children` appears.
    pub(crate) fn refs(&self, out: &mut Vec<Ref>, sum_children: &mut bool) {
        match self {
            Term::Num(_) => {}
            Term::Ref(r) => out.push(r.clone()),
            Term::SumChildren => *sum_children = true,
            Term::Neg(inner) => inner.refs(out, sum_children),
            Term::Bin(_, left, right) => {
                left.refs(out, sum_children);
                right.refs(out, sum_children);
            }
        }
    }

    /// The value; `None` when a referenced value is empty.
    pub(crate) fn eval(
        &self,
        lookup: &dyn Fn(&Ref) -> Option<Decimal>,
        children: Option<Decimal>,
    ) -> std::result::Result<Option<Decimal>, EvalError> {
        Ok(match self {
            Term::Num(n) => Some(*n),
            Term::Ref(r) => lookup(r),
            Term::SumChildren => children,
            Term::Neg(inner) => inner.eval(lookup, children)?.map(|n| -n),
            Term::Bin(op, left, right) => {
                let (Some(a), Some(b)) =
                    (left.eval(lookup, children)?, right.eval(lookup, children)?)
                else {
                    return Ok(None);
                };
                let result = match op {
                    b'+' => a.checked_add(b),
                    b'-' => a.checked_sub(b),
                    b'*' => a.checked_mul(b),
                    _ if b.is_zero() => return Err(EvalError::DivisionByZero),
                    _ => a.checked_div(b),
                };
                Some(result.ok_or(EvalError::Overflow)?)
            }
        })
    }
}

#[cfg(test)]
#[path = "tests/formula.rs"]
mod tests;
