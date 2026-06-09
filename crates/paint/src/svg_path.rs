//! SVG `<path>` data + `points` parsing (E9-M2). A lenient, never-panicking
//! parser turning the `d` attribute's command/number micro-syntax into a flat
//! list of **absolute** `PathOp`s (relative→absolute, H/V→Line, S/T reflected
//! to full C/Q, A→a run of cubics), plus a `points` parser for polygon/polyline.

/// One absolute path op (the IR fed to a tiny-skia `PathBuilder`). All coords are
/// absolute user-space; H/V are already `LineTo`, S/T already `Cubic`/`Quad`, A
/// already a run of `CubicTo`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathOp {
    MoveTo(f32, f32),
    LineTo(f32, f32),
    /// `cx,cy, x,y`
    QuadTo(f32, f32, f32, f32),
    /// `c1x,c1y, c2x,c2y, x,y`
    CubicTo(f32, f32, f32, f32, f32, f32),
    Close,
}

/// Upper bound on emitted ops to bound a pathological `d`.
const MAX_PATH_OPS: usize = 1 << 16;

/// Cursor lexer over the SVG number micro-grammar (sign/`.` as implicit
/// separators, exponents) plus a single-char flag reader for the arc flags.
struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Lexer {
            s: s.as_bytes(),
            i: 0,
        }
    }

    /// Skip a run of separators (whitespace + commas).
    fn skip_sep(&mut self) {
        while self.i < self.s.len() {
            match self.s[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' | b',' => self.i += 1,
                _ => break,
            }
        }
    }

    /// Read a command letter (`MmLlHhVvCcSsQqTtAaZz`) if the next non-sep byte is
    /// one. Does not consume anything else.
    fn next_cmd(&mut self) -> Option<u8> {
        self.skip_sep();
        if self.i >= self.s.len() {
            return None;
        }
        let c = self.s[self.i];
        if matches!(
            c,
            b'M' | b'm'
                | b'L'
                | b'l'
                | b'H'
                | b'h'
                | b'V'
                | b'v'
                | b'C'
                | b'c'
                | b'S'
                | b's'
                | b'Q'
                | b'q'
                | b'T'
                | b't'
                | b'A'
                | b'a'
                | b'Z'
                | b'z'
        ) {
            self.i += 1;
            Some(c)
        } else {
            None
        }
    }

    /// Peek whether the next non-sep byte is a command letter (for the implicit
    /// repeat loop: stop reading operand groups when a command letter is next).
    fn peek_is_cmd(&mut self) -> bool {
        self.skip_sep();
        if self.i >= self.s.len() {
            return false;
        }
        matches!(
            self.s[self.i],
            b'M' | b'm'
                | b'L'
                | b'l'
                | b'H'
                | b'h'
                | b'V'
                | b'v'
                | b'C'
                | b'c'
                | b'S'
                | b's'
                | b'Q'
                | b'q'
                | b'T'
                | b't'
                | b'A'
                | b'a'
                | b'Z'
                | b'z'
        )
    }

    /// Scan one number per the micro-grammar; `None` at end / on a non-number
    /// byte / on a parse failure (caller stops — lenient).
    fn next_number(&mut self) -> Option<f32> {
        self.skip_sep();
        let start = self.i;
        let n = self.s.len();
        // optional leading sign
        if self.i < n && (self.s[self.i] == b'+' || self.s[self.i] == b'-') {
            self.i += 1;
        }
        let mut saw_digit = false;
        // integer digits
        while self.i < n && self.s[self.i].is_ascii_digit() {
            self.i += 1;
            saw_digit = true;
        }
        // fraction
        if self.i < n && self.s[self.i] == b'.' {
            self.i += 1;
            while self.i < n && self.s[self.i].is_ascii_digit() {
                self.i += 1;
                saw_digit = true;
            }
        }
        if !saw_digit {
            self.i = start;
            return None;
        }
        // exponent
        if self.i < n && (self.s[self.i] == b'e' || self.s[self.i] == b'E') {
            let exp_start = self.i;
            self.i += 1;
            if self.i < n && (self.s[self.i] == b'+' || self.s[self.i] == b'-') {
                self.i += 1;
            }
            let mut exp_digit = false;
            while self.i < n && self.s[self.i].is_ascii_digit() {
                self.i += 1;
                exp_digit = true;
            }
            if !exp_digit {
                // a dangling `e` is not part of the number (e.g. `5end`).
                self.i = exp_start;
            }
        }
        let slice = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        slice.parse::<f32>().ok().filter(|v| v.is_finite())
    }

    /// Read exactly one `'0'`/`'1'` flag char (the arc large-arc/sweep flags may
    /// abut the next number with no separator). `None` on anything else.
    fn next_flag(&mut self) -> Option<bool> {
        self.skip_sep();
        if self.i >= self.s.len() {
            return None;
        }
        match self.s[self.i] {
            b'0' => {
                self.i += 1;
                Some(false)
            }
            b'1' => {
                self.i += 1;
                Some(true)
            }
            _ => None,
        }
    }
}

/// Interpreter state carried while turning commands into absolute ops.
struct PathState {
    cur: (f32, f32),
    start: (f32, f32),
    /// 2nd ctrl of the last C/S (for S reflection); `None` if last cmd wasn't C/S.
    prev_cubic_ctrl: Option<(f32, f32)>,
    /// ctrl of the last Q/T (for T reflection); `None` if last cmd wasn't Q/T.
    prev_quad_ctrl: Option<(f32, f32)>,
    started: bool,
    ops: Vec<PathOp>,
}

impl PathState {
    fn new() -> Self {
        PathState {
            cur: (0.0, 0.0),
            start: (0.0, 0.0),
            prev_cubic_ctrl: None,
            prev_quad_ctrl: None,
            started: false,
            ops: Vec::new(),
        }
    }

    /// Ensure a subpath has been opened before a draw op (leading non-M command
    /// per spec: synthesize a `MoveTo(cur)`).
    fn ensure_started(&mut self) {
        if !self.started {
            self.ops.push(PathOp::MoveTo(self.cur.0, self.cur.1));
            self.start = self.cur;
            self.started = true;
        }
    }
}

/// Parse the `d` attribute into a flat absolute op list. LENIENT: parses every
/// valid prefix and stops at the first malformed token (never panics, never
/// errors). An empty/whitespace `d` → empty Vec.
pub fn parse_path_data(d: &str) -> Vec<PathOp> {
    let mut lx = Lexer::new(d);
    let mut st = PathState::new();

    while let Some(cmd) = lx.next_cmd() {
        // M/m: first group is a move; subsequent implicit groups become L/l.
        // Z/z take no operands. All others loop reading operand groups until the
        // next token is a command letter or the stream ends.
        match cmd {
            b'Z' | b'z' => {
                if let Some(last) = st.ops.last() {
                    if !matches!(last, PathOp::MoveTo(..) | PathOp::Close) {
                        st.ops.push(PathOp::Close);
                    }
                }
                st.cur = st.start;
                st.prev_cubic_ctrl = None;
                st.prev_quad_ctrl = None;
            }
            _ => {
                let mut first = true;
                loop {
                    if st.ops.len() >= MAX_PATH_OPS {
                        return st.ops;
                    }
                    if !apply_group(&mut lx, &mut st, cmd, first) {
                        // malformed mid-group OR no more groups → done with cmd.
                        // `apply_group` only returns false when it couldn't read
                        // a full group; for a clean group it returns true.
                        break;
                    }
                    first = false;
                    if lx.peek_is_cmd() {
                        break;
                    }
                    if lx.skip_sep_at_end() {
                        break;
                    }
                }
            }
        }
    }
    st.ops
}

impl Lexer<'_> {
    /// True if the stream is exhausted (only separators remain).
    fn skip_sep_at_end(&mut self) -> bool {
        self.skip_sep();
        self.i >= self.s.len()
    }
}

/// Read and emit one operand group for `cmd`. Returns `true` if a full group was
/// read & emitted, `false` if the group couldn't be read (malformed / no more
/// numbers) — the caller then stops this command. `first` is the first group of
/// the command (an `M`/`m` first group is a move; later ones are lines).
fn apply_group(lx: &mut Lexer, st: &mut PathState, cmd: u8, first: bool) -> bool {
    let rel = cmd.is_ascii_lowercase();
    let cx = st.cur.0;
    let cy = st.cur.1;
    match cmd {
        b'M' | b'm' => {
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            if first {
                st.ops.push(PathOp::MoveTo(px, py));
                st.start = (px, py);
                st.started = true;
            } else {
                // implicit L/l after the first M/m group.
                st.ops.push(PathOp::LineTo(px, py));
            }
            st.cur = (px, py);
            st.prev_cubic_ctrl = None;
            st.prev_quad_ctrl = None;
        }
        b'L' | b'l' => {
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            st.ensure_started();
            st.ops.push(PathOp::LineTo(px, py));
            st.cur = (px, py);
            st.prev_cubic_ctrl = None;
            st.prev_quad_ctrl = None;
        }
        b'H' | b'h' => {
            let Some(x) = lx.next_number() else {
                return false;
            };
            let px = if rel { cx + x } else { x };
            st.ensure_started();
            st.ops.push(PathOp::LineTo(px, cy));
            st.cur = (px, cy);
            st.prev_cubic_ctrl = None;
            st.prev_quad_ctrl = None;
        }
        b'V' | b'v' => {
            let Some(y) = lx.next_number() else {
                return false;
            };
            let py = if rel { cy + y } else { y };
            st.ensure_started();
            st.ops.push(PathOp::LineTo(cx, py));
            st.cur = (cx, py);
            st.prev_cubic_ctrl = None;
            st.prev_quad_ctrl = None;
        }
        b'C' | b'c' => {
            let Some(x1) = lx.next_number() else {
                return false;
            };
            let Some(y1) = lx.next_number() else {
                return false;
            };
            let Some(x2) = lx.next_number() else {
                return false;
            };
            let Some(y2) = lx.next_number() else {
                return false;
            };
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (c1x, c1y) = if rel { (cx + x1, cy + y1) } else { (x1, y1) };
            let (c2x, c2y) = if rel { (cx + x2, cy + y2) } else { (x2, y2) };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            st.ensure_started();
            st.ops.push(PathOp::CubicTo(c1x, c1y, c2x, c2y, px, py));
            st.cur = (px, py);
            st.prev_cubic_ctrl = Some((c2x, c2y));
            st.prev_quad_ctrl = None;
        }
        b'S' | b's' => {
            let Some(x2) = lx.next_number() else {
                return false;
            };
            let Some(y2) = lx.next_number() else {
                return false;
            };
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (c2x, c2y) = if rel { (cx + x2, cy + y2) } else { (x2, y2) };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            // c1 = reflection of the prior cubic ctrl about cur, else cur.
            let (c1x, c1y) = match st.prev_cubic_ctrl {
                Some((rx, ry)) => (2.0 * cx - rx, 2.0 * cy - ry),
                None => (cx, cy),
            };
            st.ensure_started();
            st.ops.push(PathOp::CubicTo(c1x, c1y, c2x, c2y, px, py));
            st.cur = (px, py);
            st.prev_cubic_ctrl = Some((c2x, c2y));
            st.prev_quad_ctrl = None;
        }
        b'Q' | b'q' => {
            let Some(x1) = lx.next_number() else {
                return false;
            };
            let Some(y1) = lx.next_number() else {
                return false;
            };
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (qx, qy) = if rel { (cx + x1, cy + y1) } else { (x1, y1) };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            st.ensure_started();
            st.ops.push(PathOp::QuadTo(qx, qy, px, py));
            st.cur = (px, py);
            st.prev_quad_ctrl = Some((qx, qy));
            st.prev_cubic_ctrl = None;
        }
        b'T' | b't' => {
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            let (qx, qy) = match st.prev_quad_ctrl {
                Some((rx, ry)) => (2.0 * cx - rx, 2.0 * cy - ry),
                None => (cx, cy),
            };
            st.ensure_started();
            st.ops.push(PathOp::QuadTo(qx, qy, px, py));
            st.cur = (px, py);
            st.prev_quad_ctrl = Some((qx, qy));
            st.prev_cubic_ctrl = None;
        }
        b'A' | b'a' => {
            let Some(rx) = lx.next_number() else {
                return false;
            };
            let Some(ry) = lx.next_number() else {
                return false;
            };
            let Some(phi) = lx.next_number() else {
                return false;
            };
            let Some(large) = lx.next_flag() else {
                return false;
            };
            let Some(sweep) = lx.next_flag() else {
                return false;
            };
            let Some(x) = lx.next_number() else {
                return false;
            };
            let Some(y) = lx.next_number() else {
                return false;
            };
            let (px, py) = if rel { (cx + x, cy + y) } else { (x, y) };
            st.ensure_started();
            arc_to_cubics(cx, cy, rx, ry, phi, large, sweep, px, py, &mut st.ops);
            st.cur = (px, py);
            st.prev_cubic_ctrl = None;
            st.prev_quad_ctrl = None;
        }
        _ => return false,
    }
    true
}

/// Append an SVG elliptical arc (absolute endpoints) as a run of `CubicTo` ops,
/// via the W3C SVG endpoint→center parameterization (impl-notes §F.6) split into
/// ≤90° cubic segments. Degenerate cases fall back to a single `LineTo` (or
/// nothing). Never panics.
#[allow(clippy::too_many_arguments)]
fn arc_to_cubics(
    x1: f32,
    y1: f32,
    rx: f32,
    ry: f32,
    x_axis_rotation_deg: f32,
    large_arc: bool,
    sweep: bool,
    x2: f32,
    y2: f32,
    ops: &mut Vec<PathOp>,
) {
    // same start/end → arc omitted entirely.
    if (x1 - x2).abs() < f32::EPSILON && (y1 - y2).abs() < f32::EPSILON {
        return;
    }
    let mut rx = rx.abs();
    let mut ry = ry.abs();
    // zero radius → straight line.
    if rx == 0.0 || ry == 0.0 {
        ops.push(PathOp::LineTo(x2, y2));
        return;
    }

    let phi = x_axis_rotation_deg.to_radians();
    let (cos_p, sin_p) = (phi.cos(), phi.sin());

    // Step 1: midpoint, rotated into the ellipse frame (F.6.5.1).
    let dx = (x1 - x2) / 2.0;
    let dy = (y1 - y2) / 2.0;
    let x1p = cos_p * dx + sin_p * dy;
    let y1p = -sin_p * dx + cos_p * dy;

    let mut rx2 = rx * rx;
    let mut ry2 = ry * ry;
    let x1p2 = x1p * x1p;
    let y1p2 = y1p * y1p;

    // Step 2: scale radii up if too small to span (F.6.6.2).
    let lambda = x1p2 / rx2 + y1p2 / ry2;
    if lambda > 1.0 {
        let k = lambda.sqrt();
        rx *= k;
        ry *= k;
        rx2 = rx * rx;
        ry2 = ry * ry;
    }

    // Step 3: center in the ellipse frame (F.6.5.2).
    let num = (rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2).max(0.0);
    let den = rx2 * y1p2 + ry2 * x1p2;
    let sign = if large_arc == sweep { -1.0 } else { 1.0 };
    let coef = if den == 0.0 {
        0.0
    } else {
        sign * (num / den).sqrt()
    };
    let cxp = coef * rx * y1p / ry;
    let cyp = -coef * ry * x1p / rx;

    // Step 4: center back to user space (F.6.5.3).
    let cx = cos_p * cxp - sin_p * cyp + (x1 + x2) / 2.0;
    let cy = sin_p * cxp + cos_p * cyp + (y1 + y2) / 2.0;

    // Step 5: start angle θ1 and sweep Δθ (F.6.5.5/6).
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let dot = ux * vx + uy * vy;
        let len = ((ux * ux + uy * uy) * (vx * vx + vy * vy)).sqrt();
        let mut a = if len == 0.0 {
            0.0
        } else {
            (dot / len).clamp(-1.0, 1.0).acos()
        };
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let ux = (x1p - cxp) / rx;
    let uy = (y1p - cyp) / ry;
    let vx = (-x1p - cxp) / rx;
    let vy = (-y1p - cyp) / ry;
    let theta1 = ang(1.0, 0.0, ux, uy);
    let mut dtheta = ang(ux, uy, vx, vy);
    let two_pi = std::f32::consts::TAU;
    if !sweep && dtheta > 0.0 {
        dtheta -= two_pi;
    }
    if sweep && dtheta < 0.0 {
        dtheta += two_pi;
    }

    // Split into ≤90° segments (F.6.4 approximation).
    let n = (dtheta.abs() / (std::f32::consts::FRAC_PI_2 + 0.001))
        .ceil()
        .max(1.0) as usize;
    let dseg = dtheta / n as f32;
    let half = dseg / 2.0;
    // per-segment tangent factor: t = (8/3)·sin²(half/2)/sin(half).
    let sin_half = half.sin();
    let t = if sin_half.abs() < 1e-9 {
        0.0
    } else {
        (8.0 / 3.0) * (half / 2.0).sin().powi(2) / sin_half
    };

    let e = |theta: f32| -> (f32, f32) {
        let (c, s) = (theta.cos(), theta.sin());
        (
            cx + rx * c * cos_p - ry * s * sin_p,
            cy + rx * c * sin_p + ry * s * cos_p,
        )
    };
    let deriv = |theta: f32| -> (f32, f32) {
        let (c, s) = (theta.cos(), theta.sin());
        (
            -rx * s * cos_p - ry * c * sin_p,
            -rx * s * sin_p + ry * c * cos_p,
        )
    };

    for k in 0..n {
        let ta = theta1 + k as f32 * dseg;
        let tb = ta + dseg;
        let (p0x, p0y) = e(ta);
        let (p3x, p3y) = e(tb);
        let (d0x, d0y) = deriv(ta);
        let (d3x, d3y) = deriv(tb);
        let c1x = p0x + t * d0x;
        let c1y = p0y + t * d0y;
        let c2x = p3x - t * d3x;
        let c2y = p3y - t * d3y;
        ops.push(PathOp::CubicTo(c1x, c1y, c2x, c2y, p3x, p3y));
    }
}

/// Parse a `points` list (`"x1,y1 x2,y2 …"`) into pairs. Lenient: stops at the
/// first unpaired/invalid number. An odd trailing number is dropped.
pub fn parse_points(s: &str) -> Vec<(f32, f32)> {
    let mut lx = Lexer::new(s);
    let mut pts = Vec::new();
    while let Some(x) = lx.next_number() {
        let Some(y) = lx.next_number() else { break }; // drop a dangling x.
        pts.push((x, y));
        if pts.len() >= MAX_PATH_OPS {
            break;
        }
    }
    pts
}

/// Turn a points list into ops (polygon closes; polyline doesn't). Caller
/// guarantees `pts.len() >= 1`.
pub fn points_to_ops(pts: &[(f32, f32)], closed: bool) -> Vec<PathOp> {
    let mut ops = Vec::with_capacity(pts.len() + 1);
    ops.push(PathOp::MoveTo(pts[0].0, pts[0].1));
    for &(x, y) in &pts[1..] {
        ops.push(PathOp::LineTo(x, y));
    }
    if closed {
        ops.push(PathOp::Close);
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn triangle_move_line_close() {
        let ops = parse_path_data("M10 10 L90 10 L90 90 Z");
        assert_eq!(
            ops,
            vec![
                PathOp::MoveTo(10.0, 10.0),
                PathOp::LineTo(90.0, 10.0),
                PathOp::LineTo(90.0, 90.0),
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn h_v_become_lineto() {
        let ops = parse_path_data("M0 0 H10 V10 H0 Z");
        assert_eq!(
            ops,
            vec![
                PathOp::MoveTo(0.0, 0.0),
                PathOp::LineTo(10.0, 0.0),
                PathOp::LineTo(10.0, 10.0),
                PathOp::LineTo(0.0, 10.0),
                PathOp::Close,
            ]
        );
    }

    #[test]
    fn cubic() {
        let ops = parse_path_data("M0 0 C0 10 10 10 10 0");
        assert_eq!(
            ops,
            vec![
                PathOp::MoveTo(0.0, 0.0),
                PathOp::CubicTo(0.0, 10.0, 10.0, 10.0, 10.0, 0.0)
            ]
        );
    }

    #[test]
    fn smooth_cubic_reflection() {
        // 2nd cubic's c1 = reflect (5,5) about (10,0) = (15,-5).
        let ops = parse_path_data("M0 0 C0 5 5 5 10 0 S15 -5 20 0");
        assert_eq!(ops.len(), 3);
        match ops[2] {
            PathOp::CubicTo(c1x, c1y, _, _, x, y) => {
                assert!(
                    approx(c1x, 15.0, 1e-4) && approx(c1y, -5.0, 1e-4),
                    "c1=({c1x},{c1y})"
                );
                assert!(approx(x, 20.0, 1e-4) && approx(y, 0.0, 1e-4));
            }
            _ => panic!("expected cubic: {:?}", ops[2]),
        }
    }

    #[test]
    fn smooth_cubic_no_prev_uses_current() {
        // S with no preceding C → c1 = current point (0,0).
        let ops = parse_path_data("M0 0 S5 5 10 0");
        assert_eq!(ops.len(), 2);
        match ops[1] {
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                assert_eq!((c1x, c1y), (0.0, 0.0));
                assert_eq!((c2x, c2y), (5.0, 5.0));
                assert_eq!((x, y), (10.0, 0.0));
            }
            _ => panic!("expected cubic"),
        }
    }

    #[test]
    fn quad_and_smooth_quad_reflection() {
        // T ctrl = reflect (5,5) about (10,0) = (15,-5).
        let ops = parse_path_data("M0 0 Q5 5 10 0 T20 0");
        assert_eq!(ops.len(), 3);
        match ops[2] {
            PathOp::QuadTo(qx, qy, x, y) => {
                assert!(
                    approx(qx, 15.0, 1e-4) && approx(qy, -5.0, 1e-4),
                    "q=({qx},{qy})"
                );
                assert!(approx(x, 20.0, 1e-4) && approx(y, 0.0, 1e-4));
            }
            _ => panic!("expected quad"),
        }
    }

    #[test]
    fn relative_to_absolute() {
        let ops = parse_path_data("m10 10 l5 0 l0 5");
        assert_eq!(
            ops,
            vec![
                PathOp::MoveTo(10.0, 10.0),
                PathOp::LineTo(15.0, 10.0),
                PathOp::LineTo(15.0, 15.0),
            ]
        );
    }

    #[test]
    fn implicit_repeat_move_becomes_line() {
        let ops = parse_path_data("M0 0 10 0 10 10");
        assert_eq!(
            ops,
            vec![
                PathOp::MoveTo(0.0, 0.0),
                PathOp::LineTo(10.0, 0.0),
                PathOp::LineTo(10.0, 10.0),
            ]
        );
    }

    #[test]
    fn number_quirks() {
        assert_eq!(parse_path_data("M1-2"), vec![PathOp::MoveTo(1.0, -2.0)]);
        assert_eq!(parse_path_data("M.5.5"), vec![PathOp::MoveTo(0.5, 0.5)]);
        assert_eq!(
            parse_path_data("M1e1 2e1"),
            vec![PathOp::MoveTo(10.0, 20.0)]
        );
    }

    #[test]
    fn arc_quarter_circle() {
        // center (0,0), from (10,0) to (0,10), sweep → one cubic.
        let ops = parse_path_data("M10 0 A10 10 0 0 1 0 10");
        assert_eq!(ops.len(), 2, "MoveTo + one cubic: {ops:?}");
        match ops[1] {
            PathOp::CubicTo(c1x, c1y, c2x, c2y, x, y) => {
                assert!(
                    approx(x, 0.0, 1e-3) && approx(y, 10.0, 1e-3),
                    "end=({x},{y})"
                );
                assert!(
                    approx(c1x, 10.0, 0.05) && approx(c1y, 5.523, 0.05),
                    "c1=({c1x},{c1y})"
                );
                assert!(
                    approx(c2x, 5.523, 0.05) && approx(c2y, 10.0, 0.05),
                    "c2=({c2x},{c2y})"
                );
            }
            _ => panic!("expected cubic: {:?}", ops[1]),
        }
    }

    #[test]
    fn arc_abutted_flags() {
        // flags abut: "A10 10 0 011 1" → large=0, sweep=1, endpoint (1,1).
        let ops = parse_path_data("M0 0 A10 10 0 011 1");
        assert!(ops.len() >= 2, "arc emitted cubics: {ops:?}");
        if let Some(PathOp::CubicTo(_, _, _, _, x, y)) = ops.last() {
            assert!(
                approx(*x, 1.0, 1e-3) && approx(*y, 1.0, 1e-3),
                "end=({x},{y})"
            );
        } else {
            panic!("expected trailing cubic: {ops:?}");
        }
    }

    #[test]
    fn arc_degenerate_zero_radius_is_line() {
        let ops = parse_path_data("M0 0 A0 5 0 0 1 10 0");
        assert_eq!(
            ops,
            vec![PathOp::MoveTo(0.0, 0.0), PathOp::LineTo(10.0, 0.0)]
        );
    }

    #[test]
    fn arc_degenerate_same_point_skipped() {
        let ops = parse_path_data("M0 0 A5 5 0 0 1 0 0");
        assert_eq!(ops, vec![PathOp::MoveTo(0.0, 0.0)]);
    }

    #[test]
    fn malformed_parses_valid_prefix() {
        let ops = parse_path_data("M10 10 Lfoo");
        assert_eq!(ops, vec![PathOp::MoveTo(10.0, 10.0)]);
    }

    #[test]
    fn empty_is_empty() {
        assert!(parse_path_data("").is_empty());
        assert!(parse_path_data("   \t\n ").is_empty());
    }

    #[test]
    fn move_only_is_just_moveto() {
        assert_eq!(parse_path_data("M5 5"), vec![PathOp::MoveTo(5.0, 5.0)]);
    }

    #[test]
    fn points_basic() {
        assert_eq!(
            parse_points("50,5 90,90 10,90"),
            vec![(50.0, 5.0), (90.0, 90.0), (10.0, 90.0)]
        );
    }

    #[test]
    fn points_sign_separator() {
        assert_eq!(parse_points("0,0 1-2"), vec![(0.0, 0.0), (1.0, -2.0)]);
    }

    #[test]
    fn points_odd_trailing_dropped() {
        assert_eq!(parse_points("0,0 5"), vec![(0.0, 0.0)]);
    }

    #[test]
    fn points_to_ops_polygon_closes() {
        let ops = points_to_ops(&[(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)], true);
        assert_eq!(*ops.last().unwrap(), PathOp::Close);
        assert_eq!(ops[0], PathOp::MoveTo(0.0, 0.0));
    }

    #[test]
    fn points_to_ops_polyline_no_close() {
        let ops = points_to_ops(&[(0.0, 0.0), (10.0, 0.0)], false);
        assert!(!ops.iter().any(|o| matches!(o, PathOp::Close)));
    }
}
