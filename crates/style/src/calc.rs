//! `calc()` evaluation (E13-M2). A `calc(...)` reduces to a linear form
//! `a*px + b*%`. We re-tokenize the function's raw argument string (the parser
//! stores it verbatim) and run a small recursive-descent evaluator.
//!
//! `em`/`rem` fold to px via the caller-supplied bases. Type errors
//! (e.g. `% * %`), division by zero, unknown units, missing operators, and
//! over-deep nesting all yield `None` (the declaration is then left unchanged).

use starfish_css::tokenizer::{Token, Tokenizer};

use crate::computed::Length;

/// A calc() result in linear form: `px + percent% * cb`.
#[derive(Clone, Copy)]
pub(crate) struct CalcVal {
    pub px: f32,
    pub percent: f32,
}

/// Normalize a (px, percent) pair into the narrowest `Length`. A pure-px or
/// pure-percent calc collapses to `Px`/`Percent` so that `calc(10px)` and
/// `calc(50%)` are byte-identical to a plain `10px` / `50%`.
pub(crate) fn make_length(v: CalcVal) -> Length {
    if v.percent == 0.0 {
        Length::Px(v.px)
    } else if v.px == 0.0 {
        Length::Percent(v.percent)
    } else {
        Length::Calc {
            px: v.px,
            percent: v.percent,
        }
    }
}

/// An evaluated term: a linear (px, percent) value plus whether it is a pure
/// unitless number (which alone may scale/divide in `*`/`/`).
#[derive(Clone, Copy)]
struct Term {
    px: f32,
    percent: f32,
    pure: bool,
}

/// Evaluate a `calc()` raw-argument string to its linear form. `None` on any
/// type error, division by zero, bad unit, missing operator, or > 32 deep.
pub(crate) fn eval_calc(raw_args: &str, em_basis: f32, rem: f32) -> Option<CalcVal> {
    let toks = lex(raw_args);
    let mut p = Eval {
        toks: &toks,
        pos: 0,
        em_basis,
        rem,
    };
    let t = p.sum(0)?;
    // Trailing tokens (other than whitespace) → malformed.
    p.skip_ws();
    if p.pos != p.toks.len() {
        return None;
    }
    Some(CalcVal {
        px: t.px,
        percent: t.percent,
    })
}

/// Tokenize a string fully (dropping the trailing Eof).
fn lex(s: &str) -> Vec<Token> {
    let mut tz = Tokenizer::new(s);
    let mut out = Vec::new();
    loop {
        let tok = tz.next_token();
        if tok == Token::Eof {
            break;
        }
        out.push(tok);
    }
    out
}

struct Eval<'a> {
    toks: &'a [Token],
    pos: usize,
    em_basis: f32,
    rem: f32,
}

impl Eval<'_> {
    fn skip_ws(&mut self) {
        while matches!(self.toks.get(self.pos), Some(Token::Whitespace)) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.pos)
    }

    /// `sum := product ( WS ('+'|'-') WS product )*`
    fn sum(&mut self, depth: usize) -> Option<Term> {
        if depth > 32 {
            return None;
        }
        self.skip_ws();
        let mut acc = self.product(depth)?;
        loop {
            self.skip_ws();
            // A lone `-` lexes as `Ident("-")` (it is an ident-start char and
            // not followed by a digit); `+` is a `Delim`. `-20px` (no space) is
            // a negative `Dimension`, handled in `value`.
            let op = match self.peek() {
                Some(Token::Delim('+')) => '+',
                Some(Token::Ident(s)) if s == "-" => '-',
                _ => break,
            };
            self.pos += 1;
            self.skip_ws();
            let rhs = self.product(depth)?;
            // `+`/`-` are component-wise; purity is lost on combination.
            if op == '+' {
                acc = Term {
                    px: acc.px + rhs.px,
                    percent: acc.percent + rhs.percent,
                    pure: acc.pure && rhs.pure,
                };
            } else {
                acc = Term {
                    px: acc.px - rhs.px,
                    percent: acc.percent - rhs.percent,
                    pure: acc.pure && rhs.pure,
                };
            }
        }
        Some(acc)
    }

    /// `product := value ( ('*'|'/') value )*` (no surrounding WS required).
    fn product(&mut self, depth: usize) -> Option<Term> {
        let mut acc = self.value(depth)?;
        loop {
            self.skip_ws();
            let op = match self.peek() {
                Some(Token::Delim('*')) => '*',
                Some(Token::Delim('/')) => '/',
                _ => break,
            };
            self.pos += 1;
            self.skip_ws();
            let rhs = self.value(depth)?;
            if op == '*' {
                // One side must be a pure number; scale the other.
                if acc.pure {
                    acc = scale(rhs, acc.px);
                } else if rhs.pure {
                    acc = scale(acc, rhs.px);
                } else {
                    return None; // px*%, %*%, … → type error
                }
            } else {
                // Divisor must be a pure non-zero number.
                if rhs.pure && rhs.px != 0.0 {
                    acc = scale(acc, 1.0 / rhs.px);
                } else {
                    return None;
                }
            }
        }
        Some(acc)
    }

    /// `value := Number | Dimension(px/em/rem) | Percentage | '(' sum ')'
    ///         | Function("calc")(recurse on its raw_args)`
    fn value(&mut self, depth: usize) -> Option<Term> {
        self.skip_ws();
        let tok = self.peek()?.clone();
        match tok {
            Token::Number(n) => {
                self.pos += 1;
                Some(Term {
                    px: n,
                    percent: 0.0,
                    pure: true,
                })
            }
            Token::Percentage(p) => {
                self.pos += 1;
                Some(Term {
                    px: 0.0,
                    percent: p,
                    pure: false,
                })
            }
            Token::Dimension { value, unit } => {
                self.pos += 1;
                let px = match unit.as_str() {
                    "px" => value,
                    "em" => value * self.em_basis,
                    "rem" => value * self.rem,
                    _ => return None,
                };
                Some(Term {
                    px,
                    percent: 0.0,
                    pure: false,
                })
            }
            Token::LeftParen => {
                self.pos += 1;
                let inner = self.sum(depth + 1)?;
                self.skip_ws();
                if !matches!(self.peek(), Some(Token::RightParen)) {
                    return None;
                }
                self.pos += 1;
                Some(inner)
            }
            // A nested `calc(` opens a group too; the tokenizer emits a
            // `Function("calc")` followed by its args, then a matching paren.
            Token::Function(name) if name.eq_ignore_ascii_case("calc") => {
                self.pos += 1;
                let inner = self.sum(depth + 1)?;
                self.skip_ws();
                if !matches!(self.peek(), Some(Token::RightParen)) {
                    return None;
                }
                self.pos += 1;
                Some(inner)
            }
            _ => None,
        }
    }
}

/// Scale a term's px+percent by a pure factor; the result is no longer pure.
fn scale(t: Term, k: f32) -> Term {
    Term {
        px: t.px * k,
        percent: t.percent * k,
        pure: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> Option<(f32, f32)> {
        eval_calc(s, 16.0, 16.0).map(|v| (v.px, v.percent))
    }

    #[test]
    fn sum_percent_minus_px() {
        assert_eq!(ev("100% - 20px"), Some((-20.0, 100.0)));
    }

    #[test]
    fn sum_percent_plus_px() {
        assert_eq!(ev("50% + 10px"), Some((10.0, 50.0)));
    }

    #[test]
    fn nested() {
        // calc(calc(50% - 10px) + 5px) → -5px + 50%
        assert_eq!(ev("calc(50% - 10px) + 5px"), Some((-5.0, 50.0)));
    }

    #[test]
    fn parens() {
        assert_eq!(ev("(50% - 10px) + 5px"), Some((-5.0, 50.0)));
    }

    #[test]
    fn div_to_pure_percent() {
        assert_eq!(ev("100% / 2"), Some((0.0, 50.0)));
    }

    #[test]
    fn em_folds_to_px() {
        assert_eq!(ev("10px + 1em"), Some((26.0, 0.0)));
    }

    #[test]
    fn mul_by_number() {
        assert_eq!(ev("10px * 3"), Some((30.0, 0.0)));
        assert_eq!(ev("3 * 10px"), Some((30.0, 0.0)));
    }

    #[test]
    fn type_error_percent_times_percent() {
        assert_eq!(ev("50% * 50%"), None);
        assert_eq!(ev("10px * 2px"), None);
    }

    #[test]
    fn div_by_zero() {
        assert_eq!(ev("10px / 0"), None);
    }

    #[test]
    fn missing_operator() {
        assert_eq!(ev("10px 20px"), None);
    }

    #[test]
    fn bad_unit() {
        assert_eq!(ev("10vh + 5px"), None);
    }

    #[test]
    fn make_length_normalizes() {
        assert_eq!(
            make_length(CalcVal { px: 10.0, percent: 0.0 }),
            Length::Px(10.0)
        );
        assert_eq!(
            make_length(CalcVal { px: 0.0, percent: 50.0 }),
            Length::Percent(50.0)
        );
        assert_eq!(
            make_length(CalcVal { px: -20.0, percent: 100.0 }),
            Length::Calc { px: -20.0, percent: 100.0 }
        );
    }
}
