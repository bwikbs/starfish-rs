//! CSS math function evaluation: `calc()` (E13-M2) plus `min()`/`max()`/
//! `clamp()`/`round()`/`mod()`/`rem()` (E24-M1). A linear-only expression
//! reduces to `a*px + b*%`; a comparison function mixing px and % survives as
//! a [`MathExpr`] tree resolved at layout time. We re-tokenize the function's
//! raw argument string (the parser stores it verbatim) and run a small
//! recursive-descent evaluator.
//!
//! `em`/`rem` fold to px via the caller-supplied bases. Type errors
//! (e.g. `% * %`), division by zero, unknown units, missing operators, bad
//! arity, and over-deep nesting all yield `None` (the declaration is then
//! left unchanged).

use std::rc::Rc;

use starfish_css::tokenizer::{Token, Tokenizer};

use crate::computed::Length;
use crate::Viewport;

/// A math-function expression that could not be folded at parse time (it
/// compares px against %, so the winner depends on the resolution basis).
/// Resolved against the containing-block basis at layout time.
#[derive(Debug, Clone, PartialEq)]
pub enum MathExpr {
    /// A linear `px + percent% * basis` leaf.
    Leaf {
        px: f32,
        percent: f32,
    },
    Min(Vec<MathExpr>),
    Max(Vec<MathExpr>),
    /// `[min, val, max]`.
    Clamp(Box<[MathExpr; 3]>),
    /// `calc(math ± linear ± …)` — addition of sub-expressions.
    Sum(Vec<MathExpr>),
    /// `k * math` (kept un-distributed: a negative `k` would flip min/max).
    Scale(f32, Box<MathExpr>),
}

impl MathExpr {
    /// Resolve to px against the percentage `basis`.
    pub fn resolve(&self, basis: f32) -> f32 {
        match self {
            MathExpr::Leaf { px, percent } => px + percent / 100.0 * basis,
            MathExpr::Min(args) => args
                .iter()
                .map(|a| a.resolve(basis))
                .fold(f32::INFINITY, f32::min),
            MathExpr::Max(args) => args
                .iter()
                .map(|a| a.resolve(basis))
                .fold(f32::NEG_INFINITY, f32::max),
            // CSS clamp(MIN, VAL, MAX) = max(MIN, min(VAL, MAX)).
            MathExpr::Clamp(b) => {
                let [mn, val, mx] = &**b;
                mn.resolve(basis)
                    .max(val.resolve(basis).min(mx.resolve(basis)))
            }
            MathExpr::Sum(args) => args.iter().map(|a| a.resolve(basis)).sum(),
            MathExpr::Scale(k, inner) => k * inner.resolve(basis),
        }
    }

    /// Serialize for `getComputedStyle`, e.g. `min(300px, 50%)`.
    pub fn to_css_string(&self) -> String {
        match self {
            MathExpr::Leaf { px, percent } => {
                if *percent == 0.0 {
                    format!("{}px", fmt_num(*px))
                } else if *px == 0.0 {
                    format!("{}%", fmt_num(*percent))
                } else {
                    format!("calc({}% + {}px)", fmt_num(*percent), fmt_num(*px))
                }
            }
            MathExpr::Min(args) => format!("min({})", join_args(args)),
            MathExpr::Max(args) => format!("max({})", join_args(args)),
            MathExpr::Clamp(b) => format!("clamp({})", join_args(&b[..])),
            MathExpr::Sum(args) => {
                let parts: Vec<String> = args.iter().map(|a| a.to_css_string()).collect();
                format!("calc({})", parts.join(" + "))
            }
            MathExpr::Scale(k, inner) => {
                format!("calc({} * {})", fmt_num(*k), inner.to_css_string())
            }
        }
    }
}

fn join_args(args: &[MathExpr]) -> String {
    let parts: Vec<String> = args.iter().map(|a| a.to_css_string()).collect();
    parts.join(", ")
}

/// Format a number without a trailing `.0` ("16" not "16.0"; "0.5" stays).
fn fmt_num(n: f32) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

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

/// An evaluated term: either a linear (px, percent) value plus whether it is a
/// pure unitless number (which alone may scale/divide in `*`/`/`), or an
/// unfoldable math-function tree.
#[derive(Clone)]
enum Term {
    Lin { px: f32, percent: f32, pure: bool },
    Tree(MathExpr),
}

impl Term {
    fn into_expr(self) -> MathExpr {
        match self {
            Term::Lin { px, percent, .. } => MathExpr::Leaf { px, percent },
            Term::Tree(e) => e,
        }
    }
}

/// True for the function names handled by [`eval_math_fn`].
pub(crate) fn is_math_fn(name: &str) -> bool {
    ["calc", "min", "max", "clamp", "round", "mod", "rem"]
        .iter()
        .any(|f| name.eq_ignore_ascii_case(f))
}

/// Evaluate a math function's raw-argument string to a `Length`. A linear
/// result normalizes through [`make_length`] (pure px/% collapse to
/// `Px`/`Percent`); an unfoldable comparison becomes `Length::Math`. `None` on
/// any type error, division by zero, bad unit/arity, missing operator, or
/// > 32 deep — the declaration is then ignored.
pub(crate) fn eval_math_fn(
    name: &str,
    raw_args: &str,
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<Length> {
    let toks = lex(raw_args);
    let mut p = Eval {
        toks: &toks,
        pos: 0,
        em_basis,
        rem,
        vp,
    };
    let t = if name.eq_ignore_ascii_case("calc") {
        let t = p.sum(0)?;
        // Trailing tokens (other than whitespace) → malformed.
        p.skip_ws();
        if p.pos != p.toks.len() {
            return None;
        }
        t
    } else {
        p.math_fn_args(name, 0, true)?
    };
    Some(match t {
        Term::Lin { px, percent, .. } => make_length(CalcVal { px, percent }),
        Term::Tree(e) => Length::Math(Rc::new(e)),
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
    vp: Viewport,
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
            // `+`/`-` are component-wise on linear terms; purity is lost on
            // combination. A tree on either side becomes a `Sum` node.
            acc = match (acc, rhs) {
                (
                    Term::Lin {
                        px: apx,
                        percent: apct,
                        pure: apure,
                    },
                    Term::Lin {
                        px: bpx,
                        percent: bpct,
                        pure: bpure,
                    },
                ) => {
                    if op == '+' {
                        Term::Lin {
                            px: apx + bpx,
                            percent: apct + bpct,
                            pure: apure && bpure,
                        }
                    } else {
                        Term::Lin {
                            px: apx - bpx,
                            percent: apct - bpct,
                            pure: apure && bpure,
                        }
                    }
                }
                (a, b) => {
                    let be = if op == '-' {
                        MathExpr::Scale(-1.0, Box::new(b.into_expr()))
                    } else {
                        b.into_expr()
                    };
                    let terms = match a.into_expr() {
                        MathExpr::Sum(mut v) => {
                            v.push(be);
                            v
                        }
                        ae => vec![ae, be],
                    };
                    Term::Tree(MathExpr::Sum(terms))
                }
            };
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
                if let Some(k) = pure_of(&acc) {
                    acc = scale(rhs, k);
                } else if let Some(k) = pure_of(&rhs) {
                    acc = scale(acc, k);
                } else {
                    return None; // px*%, %*%, … → type error
                }
            } else {
                // Divisor must be a pure non-zero number.
                match pure_of(&rhs) {
                    Some(k) if k != 0.0 => acc = scale(acc, 1.0 / k),
                    _ => return None,
                }
            }
        }
        Some(acc)
    }

    /// `value := Number | Dimension(px/em/rem) | Percentage | '(' sum ')'
    ///         | Function(calc|min|max|clamp|round|mod|rem)(recurse)`
    fn value(&mut self, depth: usize) -> Option<Term> {
        self.skip_ws();
        let tok = self.peek()?.clone();
        match tok {
            Token::Number(n) => {
                self.pos += 1;
                Some(Term::Lin {
                    px: n,
                    percent: 0.0,
                    pure: true,
                })
            }
            Token::Percentage(p) => {
                self.pos += 1;
                Some(Term::Lin {
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
                    // viewport units (E13-M3) fold to px like em/rem.
                    "vw" => value / 100.0 * self.vp.width,
                    "vh" => value / 100.0 * self.vp.height,
                    "vmin" => value / 100.0 * self.vp.width.min(self.vp.height),
                    "vmax" => value / 100.0 * self.vp.width.max(self.vp.height),
                    // container query units (E25-M1).
                    "cqw" | "cqi" => value / 100.0 * self.vp.container_inline,
                    "cqh" | "cqb" => value / 100.0 * self.vp.container_block,
                    "cqmin" => value / 100.0 * self.vp.container_inline.min(self.vp.container_block),
                    "cqmax" => value / 100.0 * self.vp.container_inline.max(self.vp.container_block),
                    _ => return None,
                };
                Some(Term::Lin {
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
            // A nested math function (E24-M1): comma-separated args, closed by
            // a matching paren.
            Token::Function(name) if is_math_fn(&name) => {
                self.pos += 1;
                self.math_fn_args(&name, depth + 1, false)
            }
            _ => None,
        }
    }

    /// Parse the argument list of `min`/`max`/`clamp`/`round`/`mod`/`rem` (the
    /// function name token is already consumed). `until_eof` distinguishes the
    /// top-level call (args end at end-of-input) from a nested call (args end
    /// at the matching `)`). `round` accepts an optional leading strategy
    /// ident — only `nearest` is modeled; others drop the declaration.
    fn math_fn_args(&mut self, name: &str, depth: usize, until_eof: bool) -> Option<Term> {
        if depth > 32 {
            return None;
        }
        if name.eq_ignore_ascii_case("round") {
            self.skip_ws();
            if let Some(Token::Ident(s)) = self.peek() {
                if !s.eq_ignore_ascii_case("nearest") {
                    return None; // up/down/to-zero (and typos) → unsupported
                }
                self.pos += 1;
                self.skip_ws();
                if !matches!(self.peek(), Some(Token::Comma)) {
                    return None;
                }
                self.pos += 1;
            }
        }
        let mut args = vec![self.sum(depth)?];
        loop {
            self.skip_ws();
            match self.peek() {
                Some(Token::Comma) => {
                    self.pos += 1;
                    args.push(self.sum(depth)?);
                }
                Some(Token::RightParen) if !until_eof => {
                    self.pos += 1;
                    break;
                }
                None if until_eof => break,
                _ => return None,
            }
        }
        build_math_fn(name, args)
    }
}

/// Apply arity checks + parse-time folding for one math function call. Pure-px
/// (or pure-%) comparisons fold to a linear term so `Length::Math` only
/// survives when px and % actually compete.
fn build_math_fn(name: &str, mut args: Vec<Term>) -> Option<Term> {
    let lname = name.to_ascii_lowercase();
    match lname.as_str() {
        "min" | "max" => {
            if args.len() == 1 {
                return args.pop();
            }
            let is_min = lname == "min";
            if let Some(folded) = fold_compare(&args, is_min) {
                return Some(folded);
            }
            let exprs: Vec<MathExpr> = args.into_iter().map(Term::into_expr).collect();
            Some(Term::Tree(if is_min {
                MathExpr::Min(exprs)
            } else {
                MathExpr::Max(exprs)
            }))
        }
        "clamp" => {
            if args.len() != 3 {
                return None;
            }
            if let [Term::Lin {
                px: n,
                percent: np,
                pure: p1,
            }, Term::Lin {
                px: v,
                percent: vp,
                pure: p2,
            }, Term::Lin {
                px: x,
                percent: xp,
                pure: p3,
            }] = args[..]
            {
                // All-px (or all-%) → fold per CSS clamp = max(MIN, min(VAL, MAX)).
                if np == 0.0 && vp == 0.0 && xp == 0.0 {
                    return Some(Term::Lin {
                        px: n.max(v.min(x)),
                        percent: 0.0,
                        pure: p1 && p2 && p3,
                    });
                }
                if n == 0.0 && v == 0.0 && x == 0.0 {
                    return Some(Term::Lin {
                        px: 0.0,
                        percent: np.max(vp.min(xp)),
                        pure: false,
                    });
                }
            }
            let mut it = args.into_iter().map(Term::into_expr);
            let arr = [it.next()?, it.next()?, it.next()?];
            Some(Term::Tree(MathExpr::Clamp(Box::new(arr))))
        }
        "round" | "mod" | "rem" => {
            if args.len() != 2 {
                return None;
            }
            // Only the all-px form folds; a % participant is deferred (the
            // declaration is dropped rather than mis-resolved).
            let (a, b, pure) = match args[..] {
                [Term::Lin {
                    px: a,
                    percent: 0.0,
                    pure: pa,
                }, Term::Lin {
                    px: b,
                    percent: 0.0,
                    pure: pb,
                }] => (a, b, pa && pb),
                _ => return None,
            };
            if b == 0.0 {
                return None;
            }
            let px = match lname.as_str() {
                "round" => (a / b).round() * b,
                "mod" => a - b * (a / b).floor(),
                _ => a - b * (a / b).trunc(), // rem
            };
            if !px.is_finite() {
                return None;
            }
            Some(Term::Lin {
                px,
                percent: 0.0,
                pure,
            })
        }
        _ => None,
    }
}

/// Fold a min/max over all-linear args when only one of px/% participates.
/// Returns `None` when the args mix px and % (the comparison must wait for the
/// layout-time basis) or include a tree.
fn fold_compare(args: &[Term], is_min: bool) -> Option<Term> {
    let mut all_px = true;
    let mut all_pct = true;
    let mut all_pure = true;
    for a in args {
        match a {
            Term::Lin { px, percent, pure } => {
                if *percent != 0.0 {
                    all_px = false;
                }
                if *px != 0.0 {
                    all_pct = false;
                }
                all_pure &= *pure;
            }
            Term::Tree(_) => return None,
        }
    }
    let pick = |x: f32, y: f32| if is_min { x.min(y) } else { x.max(y) };
    if all_px {
        let mut acc = match args[0] {
            Term::Lin { px, .. } => px,
            _ => unreachable!(),
        };
        for a in &args[1..] {
            if let Term::Lin { px, .. } = a {
                acc = pick(acc, *px);
            }
        }
        Some(Term::Lin {
            px: acc,
            percent: 0.0,
            pure: all_pure,
        })
    } else if all_pct {
        let mut acc = match args[0] {
            Term::Lin { percent, .. } => percent,
            _ => unreachable!(),
        };
        for a in &args[1..] {
            if let Term::Lin { percent, .. } = a {
                acc = pick(acc, *percent);
            }
        }
        Some(Term::Lin {
            px: 0.0,
            percent: acc,
            pure: false,
        })
    } else {
        None
    }
}

/// The pure-number value of a term, if it is one.
fn pure_of(t: &Term) -> Option<f32> {
    match t {
        Term::Lin { px, pure: true, .. } => Some(*px),
        _ => None,
    }
}

/// Scale a term's px+percent (or wrap a tree in `Scale`) by a pure factor;
/// the result is no longer pure.
fn scale(t: Term, k: f32) -> Term {
    match t {
        Term::Lin { px, percent, .. } => Term::Lin {
            px: px * k,
            percent: percent * k,
            pure: false,
        },
        Term::Tree(e) => Term::Tree(MathExpr::Scale(k, Box::new(e))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport::from_width(800.0)
    }

    /// Evaluate a `calc()` body to its linear `(px, percent)` form.
    fn ev(s: &str) -> Option<(f32, f32)> {
        match eval_math_fn("calc", s, 16.0, 16.0, vp())? {
            Length::Px(px) => Some((px, 0.0)),
            Length::Percent(p) => Some((0.0, p)),
            Length::Calc { px, percent } => Some((px, percent)),
            _ => None,
        }
    }

    /// Evaluate a full math function call.
    fn evf(name: &str, s: &str) -> Option<Length> {
        eval_math_fn(name, s, 16.0, 16.0, vp())
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
        // `ch` is still unmodelled (vh/vw became valid in E13-M3).
        assert_eq!(ev("10ch + 5px"), None);
    }

    #[test]
    fn viewport_units_fold_to_px() {
        // vp 800×600: 50vw = 400, 50vh = 300, 10vmin = 60, 10vmax = 80.
        assert_eq!(ev("50vw - 10px"), Some((390.0, 0.0)));
        assert_eq!(ev("50vh"), Some((300.0, 0.0)));
        assert_eq!(ev("10vmin"), Some((60.0, 0.0)));
        assert_eq!(ev("10vmax"), Some((80.0, 0.0)));
    }

    #[test]
    fn make_length_normalizes() {
        assert_eq!(
            make_length(CalcVal {
                px: 10.0,
                percent: 0.0
            }),
            Length::Px(10.0)
        );
        assert_eq!(
            make_length(CalcVal {
                px: 0.0,
                percent: 50.0
            }),
            Length::Percent(50.0)
        );
        assert_eq!(
            make_length(CalcVal {
                px: -20.0,
                percent: 100.0
            }),
            Length::Calc {
                px: -20.0,
                percent: 100.0
            }
        );
    }

    // --- E24-M1 math functions ---

    /// Unwrap a `Length::Math` or panic.
    fn math_of(l: Length) -> Rc<MathExpr> {
        match l {
            Length::Math(m) => m,
            other => panic!("expected Length::Math, got {other:?}"),
        }
    }

    #[test]
    fn clamp_px_percent_survives_and_resolves() {
        let m = math_of(evf("clamp", "200px, 50%, 600px").unwrap());
        assert_eq!(m.resolve(300.0), 200.0); // 150 clamped up to min
        assert_eq!(m.resolve(1000.0), 500.0); // 50% in range
        assert_eq!(m.resolve(2000.0), 600.0); // 1000 clamped to max
    }

    #[test]
    fn min_max_extremes() {
        let m = math_of(evf("min", "300px, 50%").unwrap());
        assert_eq!(m.resolve(400.0), 200.0); // 50% wins
        assert_eq!(m.resolve(1000.0), 300.0); // px wins
        let m = math_of(evf("max", "300px, 50%").unwrap());
        assert_eq!(m.resolve(400.0), 300.0);
        assert_eq!(m.resolve(1000.0), 500.0);
    }

    #[test]
    fn pure_px_min_folds() {
        assert_eq!(evf("min", "100px, 200px"), Some(Length::Px(100.0)));
        assert_eq!(evf("max", "100px, 200px"), Some(Length::Px(200.0)));
        assert_eq!(
            evf("clamp", "10px, 5px, 20px"),
            Some(Length::Px(10.0)) // max(MIN, min(VAL, MAX))
        );
    }

    #[test]
    fn pure_percent_min_folds() {
        assert_eq!(evf("min", "10%, 20%"), Some(Length::Percent(10.0)));
        assert_eq!(evf("max", "10%, 20%"), Some(Length::Percent(20.0)));
    }

    #[test]
    fn single_arg_min_is_the_arg() {
        assert_eq!(evf("min", "30px"), Some(Length::Px(30.0)));
        assert_eq!(evf("max", "25%"), Some(Length::Percent(25.0)));
    }

    #[test]
    fn nested_calc_inside_min() {
        // min(calc(100% - 20px), 300px) at basis 400 → min(380, 300) = 300;
        // at basis 200 → min(180, 300) = 180.
        let m = math_of(evf("min", "calc(100% - 20px), 300px").unwrap());
        assert_eq!(m.resolve(400.0), 300.0);
        assert_eq!(m.resolve(200.0), 180.0);
    }

    #[test]
    fn nested_min_inside_clamp() {
        // clamp(1rem, min(5%, 2em), 4rem) with em=rem=16 → clamp(16, min(5%, 32), 64).
        let m = math_of(evf("clamp", "1rem, min(5%, 2em), 4rem").unwrap());
        assert_eq!(m.resolve(400.0), 20.0); // min(20, 32)=20 in [16, 64]
        assert_eq!(m.resolve(100.0), 16.0); // min(5, 32)=5 → clamped to 16
        assert_eq!(m.resolve(10000.0), 32.0); // min(500, 32)=32 in range
    }

    #[test]
    fn math_fn_inside_calc_sum() {
        // calc(min(10px, 5%) + 2px): basis 100 → min(10,5)+2 = 7.
        let m = math_of(ev_len("min(10px, 5%) + 2px").unwrap());
        assert_eq!(m.resolve(100.0), 7.0);
        assert_eq!(m.resolve(1000.0), 12.0);
    }

    /// calc() body evaluated to a full `Length` (trees allowed).
    fn ev_len(s: &str) -> Option<Length> {
        eval_math_fn("calc", s, 16.0, 16.0, vp())
    }

    #[test]
    fn scale_of_tree_keeps_extremes() {
        // calc(min(100px, 50%) * 2): basis 100 → min(100,50)*2 = 100.
        let m = math_of(ev_len("min(100px, 50%) * 2").unwrap());
        assert_eq!(m.resolve(100.0), 100.0);
        assert_eq!(m.resolve(1000.0), 200.0);
        // Division by a pure number.
        let m = math_of(ev_len("min(100px, 50%) / 2").unwrap());
        assert_eq!(m.resolve(100.0), 25.0);
    }

    #[test]
    fn round_mod_rem_fold_px() {
        assert_eq!(evf("round", "105px, 10px"), Some(Length::Px(110.0)));
        assert_eq!(
            evf("round", "nearest, 105px, 10px"),
            Some(Length::Px(110.0))
        );
        assert_eq!(evf("mod", "18px, 5px"), Some(Length::Px(3.0)));
        assert_eq!(evf("rem", "18px, 5px"), Some(Length::Px(3.0)));
        // Negative dividend: mod follows the divisor sign, rem the dividend.
        assert_eq!(evf("mod", "-18px, 5px"), Some(Length::Px(2.0)));
        assert_eq!(evf("rem", "-18px, 5px"), Some(Length::Px(-3.0)));
    }

    #[test]
    fn round_unsupported_strategy_drops() {
        assert_eq!(evf("round", "up, 105px, 10px"), None);
        assert_eq!(evf("round", "to-zero, 105px, 10px"), None);
    }

    #[test]
    fn mod_with_percent_drops() {
        assert_eq!(evf("mod", "50%, 10px"), None);
        assert_eq!(evf("round", "50%, 10px"), None);
        assert_eq!(evf("mod", "10px, 0px"), None); // divisor 0
    }

    #[test]
    fn bad_arity_drops() {
        assert_eq!(evf("clamp", "1px, 2px"), None);
        assert_eq!(evf("clamp", "1px, 2px, 3px, 4px"), None);
        assert_eq!(evf("min", ""), None);
        assert_eq!(evf("mod", "10px"), None);
    }

    #[test]
    fn depth_cap_drops() {
        // 40 nested min() levels exceed the 32-deep cap.
        let mut s = String::from("10px");
        for _ in 0..40 {
            s = format!("min({s}, 50%)");
        }
        // Strip the outermost name+parens for the entry call.
        let inner = &s["min(".len()..s.len() - 1];
        assert_eq!(evf("min", inner), None);
    }

    #[test]
    fn to_css_string_forms() {
        let m = math_of(evf("min", "300px, 50%").unwrap());
        assert_eq!(m.to_css_string(), "min(300px, 50%)");
        let m = math_of(evf("clamp", "200px, 50%, 600px").unwrap());
        assert_eq!(m.to_css_string(), "clamp(200px, 50%, 600px)");
    }
}
