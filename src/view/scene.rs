//! Charts as a display list: shapes in points, relative to the chart's top
//! left corner, with colour tokens. Shells draw it with their own 2D API.
//! Points and text live in shared buffers that primitives index into, so
//! the whole scene crosses an FFI boundary as a few flat arrays.

use std::ops::Range;

use super::palette::Color;

/// `repr(C)`, so a scene's points cross an FFI boundary as float pairs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

pub const fn pt(x: f32, y: f32) -> Point {
    Point { x, y }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x0 && p.x <= self.x1 && p.y >= self.y0 && p.y <= self.y1
    }
    pub fn center_y(&self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum HAlign {
    Start = 0,
    Center = 1,
    End = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum VAlign {
    Top = 0,
    Center = 1,
    Bottom = 2,
}

/// Text sizes from the type scale: axis ticks and tooltips, and body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum TextSize {
    Small = 0,
    Body = 1,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    /// Polyline through `pts[range]`; dashed with `palette::DASH` if set.
    Line {
        range: Range<u32>,
        color: Color,
        alpha: f32,
        width: f32,
        dashed: bool,
    },
    /// Flat fill between the polyline `pts[range]` and the line `y = base`.
    Area {
        range: Range<u32>,
        base: f32,
        color: Color,
        alpha: f32,
    },
    /// Filled rectangle, or a 1 pt outline drawn inside it.
    Rect {
        rect: Rect,
        color: Color,
        alpha: f32,
        filled: bool,
    },
    /// A dot of `radius` at each of `pts[range]`.
    Dots {
        range: Range<u32>,
        radius: f32,
        color: Color,
        alpha: f32,
    },
    /// `text[range]` anchored at `at`.
    Text {
        at: Point,
        range: Range<u32>,
        size: TextSize,
        color: Color,
        h: HAlign,
        v: VAlign,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    /// The plot area inside the chart (axes and labels sit around it).
    pub plot: Rect,
    pub prims: Vec<Prim>,
    pub pts: Vec<Point>,
    pub text: String,
    /// While set, lines, areas and dots are cut to this x range.
    clip: Option<(f32, f32)>,
    scratch: Vec<Point>,
    runs: Vec<(usize, usize)>,
}

/// Point `t` of the way from `a` to `b`, exact at the ends.
fn lerp(a: Point, b: Point, t: f32) -> Point {
    if t <= 0.0 {
        a
    } else if t >= 1.0 {
        b
    } else {
        pt(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
    }
}

/// The runs of the polyline `pts` inside `x0..=x1`, with points
/// interpolated where it crosses the edges. Runs are `out[start..end]`.
fn clip_x(pts: &[Point], x0: f32, x1: f32, out: &mut Vec<Point>, runs: &mut Vec<(usize, usize)>) {
    out.clear();
    runs.clear();
    if let [p] = pts {
        if p.x >= x0 && p.x <= x1 {
            out.push(*p);
            runs.push((0, 1));
        }
        return;
    }
    let mut start: Option<usize> = None;
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let dx = b.x - a.x;
        let (t0, t1) = if dx.abs() < 1e-9 {
            if a.x < x0 || a.x > x1 {
                (1.0, 0.0)
            } else {
                (0.0, 1.0)
            }
        } else {
            let (ta, tb) = ((x0 - a.x) / dx, (x1 - a.x) / dx);
            let (lo, hi) = if ta < tb { (ta, tb) } else { (tb, ta) };
            (lo.max(0.0), hi.min(1.0))
        };
        if t0 > t1 {
            if let Some(s) = start.take() {
                runs.push((s, out.len()));
            }
            continue;
        }
        let (p, q) = (lerp(a, b, t0), lerp(a, b, t1));
        match start {
            Some(s) if out.last() != Some(&p) => {
                runs.push((s, out.len()));
                start = Some(out.len());
                out.push(p);
            }
            Some(_) => {}
            None => {
                start = Some(out.len());
                out.push(p);
            }
        }
        out.push(q);
        if t1 < 1.0
            && let Some(s) = start.take()
        {
            runs.push((s, out.len()));
        }
    }
    if let Some(s) = start {
        runs.push((s, out.len()));
    }
}

impl Scene {
    pub fn clear(&mut self, plot: Rect) {
        self.plot = plot;
        self.prims.clear();
        self.pts.clear();
        self.text.clear();
        self.clip = None;
    }

    /// Cut the lines, areas and dots that follow to `x0..=x1` (the plot),
    /// or stop cutting with `None`.
    pub fn clip_x(&mut self, range: Option<(f32, f32)>) {
        self.clip = range;
    }

    /// Calls `f` with each run of `pts` inside the clip range.
    fn clipped(&mut self, pts: &[Point], mut f: impl FnMut(&mut Scene, &[Point])) {
        let Some((x0, x1)) = self.clip else {
            f(self, pts);
            return;
        };
        let (mut out, mut runs) = (
            std::mem::take(&mut self.scratch),
            std::mem::take(&mut self.runs),
        );
        clip_x(pts, x0, x1, &mut out, &mut runs);
        for &(a, b) in &runs {
            f(self, &out[a..b]);
        }
        self.scratch = out;
        self.runs = runs;
    }

    fn push_pts(&mut self, pts: &[Point]) -> Range<u32> {
        let start = self.pts.len() as u32;
        self.pts.extend_from_slice(pts);
        start..self.pts.len() as u32
    }

    pub fn line(&mut self, pts: &[Point], width: f32, color: Color, alpha: f32) {
        self.clipped(pts, |sc, pts| match pts.len() {
            0 => {}
            // A lone sample still shows.
            1 => sc.push_dots(pts, width * 0.75, color, alpha),
            _ => {
                let range = sc.push_pts(pts);
                sc.prims.push(Prim::Line {
                    range,
                    color,
                    alpha,
                    width,
                    dashed: false,
                });
            }
        });
    }

    pub fn segment(&mut self, a: Point, b: Point, width: f32, color: Color, alpha: f32) {
        self.line(&[a, b], width, color, alpha);
    }

    pub fn dashed_hline(&mut self, x0: f32, x1: f32, y: f32, color: Color, alpha: f32) {
        let range = self.push_pts(&[pt(x0, y), pt(x1, y)]);
        self.prims.push(Prim::Line {
            range,
            color,
            alpha,
            width: super::palette::STROKE,
            dashed: true,
        });
    }

    pub fn area(&mut self, pts: &[Point], base: f32, color: Color, alpha: f32) {
        self.clipped(pts, |sc, pts| {
            if pts.len() < 2 {
                return;
            }
            let range = sc.push_pts(pts);
            sc.prims.push(Prim::Area {
                range,
                base,
                color,
                alpha,
            });
        });
    }

    pub fn fill(&mut self, rect: Rect, color: Color, alpha: f32) {
        self.prims.push(Prim::Rect {
            rect,
            color,
            alpha,
            filled: true,
        });
    }

    pub fn outline(&mut self, rect: Rect, color: Color, alpha: f32) {
        self.prims.push(Prim::Rect {
            rect,
            color,
            alpha,
            filled: false,
        });
    }

    pub fn dots(&mut self, pts: &[Point], radius: f32, color: Color, alpha: f32) {
        match self.clip {
            Some((x0, x1)) => {
                let mut kept = std::mem::take(&mut self.scratch);
                kept.clear();
                kept.extend(pts.iter().filter(|p| p.x >= x0 && p.x <= x1));
                self.push_dots(&kept, radius, color, alpha);
                self.scratch = kept;
            }
            None => self.push_dots(pts, radius, color, alpha),
        }
    }

    fn push_dots(&mut self, pts: &[Point], radius: f32, color: Color, alpha: f32) {
        if pts.is_empty() {
            return;
        }
        let range = self.push_pts(pts);
        self.prims.push(Prim::Dots {
            range,
            radius,
            color,
            alpha,
        });
    }

    pub fn text(&mut self, at: Point, s: &str, size: TextSize, color: Color, h: HAlign, v: VAlign) {
        let start = self.text.len() as u32;
        self.text.push_str(s);
        self.prims.push(Prim::Text {
            at,
            range: start..self.text.len() as u32,
            size,
            color,
            h,
            v,
        });
    }

    /// The text of a `Prim::Text`.
    pub fn str(&self, range: &Range<u32>) -> &str {
        &self.text[range.start as usize..range.end as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_cuts_at_the_edges() {
        let mut out = Vec::new();
        let mut runs = Vec::new();
        let pts = [
            pt(-10.0, 0.0),
            pt(10.0, 20.0),
            pt(30.0, 0.0),
            pt(150.0, 0.0),
        ];
        clip_x(&pts, 0.0, 100.0, &mut out, &mut runs);
        assert_eq!(runs, vec![(0, 4)]);
        assert_eq!(out[0], pt(0.0, 10.0));
        assert_eq!(out[1], pt(10.0, 20.0));
        assert_eq!(out[3], pt(100.0, 0.0));
        // Leaving and re-entering makes two runs.
        let pts = [pt(10.0, 0.0), pt(200.0, 0.0), pt(200.0, 5.0), pt(50.0, 5.0)];
        clip_x(&pts, 0.0, 100.0, &mut out, &mut runs);
        assert_eq!(runs.len(), 2);
        // Entirely outside: nothing.
        clip_x(
            &[pt(-5.0, 0.0), pt(-1.0, 0.0)],
            0.0,
            100.0,
            &mut out,
            &mut runs,
        );
        assert!(runs.is_empty());
    }
}
