//! Rectangles for sharing part of a screen: which parts of the shared windows
//! show, and greying out the rest of a frame.
// Not all of it shares windows yet on macOS.
#![cfg_attr(target_os = "macos", allow(dead_code))]

use super::{Image, Pixels};

/// A rectangle in pixels, right and bottom exclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct R {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl R {
    pub fn empty(&self) -> bool {
        self.r <= self.l || self.b <= self.t
    }

    pub fn and(&self, o: &R) -> R {
        R { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) }
    }

    pub fn union(&self, o: &R) -> R {
        R { l: self.l.min(o.l), t: self.t.min(o.t), r: self.r.max(o.r), b: self.b.max(o.b) }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }

    /// The parts of `self` outside `o`: at most four rectangles.
    pub fn minus(&self, o: &R) -> Vec<R> {
        let i = self.and(o);
        if i.empty() {
            return vec![*self];
        }
        let mut out = Vec::with_capacity(4);
        let mut push = |r: R| {
            if !r.empty() {
                out.push(r)
            }
        };
        push(R { l: self.l, t: self.t, r: self.r, b: i.t });
        push(R { l: self.l, t: i.b, r: self.r, b: self.b });
        push(R { l: self.l, t: i.t, r: i.l, b: i.b });
        push(R { l: i.r, t: i.t, r: self.r, b: i.b });
        out
    }
}

/// From a stack of windows (top first, flagged when shared), the parts of the shared ones inside
/// `area` that nothing above covers.
pub fn shown_parts(stack: &[(R, bool)], area: R) -> Vec<R> {
    let mut covered: Vec<R> = Vec::new();
    let mut shown = Vec::new();
    for &(rect, mine) in stack {
        let r = rect.and(&area);
        if r.empty() {
            continue;
        }
        if mine {
            let mut parts = vec![r];
            for c in &covered {
                parts = parts.iter().flat_map(|p| p.minus(c)).collect();
                if parts.is_empty() || parts.len() > 256 {
                    break;
                }
            }
            shown.extend(parts);
        }
        covered.push(r);
    }
    shown
}

/// The colour of hidden parts, BGRx.
pub const HIDDEN: [u8; 4] = [38, 34, 34, 0];

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// `area` of `img` with everything outside `shown` (given relative to `area`) greyed out.
pub fn crop_masked(img: &Pixels, area: R, shown: &[R]) -> Image {
    let area = area.and(&R { l: 0, t: 0, r: img.w as i32, b: img.h as i32 });
    let (w, h) = ((area.r - area.l).max(1) as usize, (area.b - area.t).max(1) as usize);
    let mut data = vec![0u8; w * h * 4];
    for px in data.as_chunks_mut::<4>().0 {
        px.copy_from_slice(&HIDDEN);
    }
    for s in shown {
        let s = s.and(&R { l: 0, t: 0, r: w as i32, b: h as i32 });
        if s.empty() {
            continue;
        }
        for y in s.t..s.b {
            let src = (area.t + y) as usize * img.stride + (area.l + s.l) as usize * 4;
            let dst = y as usize * w * 4 + s.l as usize * 4;
            let n = (s.r - s.l) as usize * 4;
            data[dst..dst + n].copy_from_slice(&img.data[src..src + n]);
        }
    }
    Image { w, h, stride: w * 4, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtracting_rectangles_leaves_the_rest() {
        let a = R { l: 0, t: 0, r: 100, b: 100 };
        let parts = a.minus(&R { l: 25, t: 25, r: 75, b: 75 });
        let area: i32 = parts.iter().map(|p| (p.r - p.l) * (p.b - p.t)).sum();
        assert_eq!(area, 100 * 100 - 50 * 50);
        assert_eq!(a.minus(&R { l: 200, t: 0, r: 300, b: 50 }), vec![a]);
        assert!(a.minus(&R { l: -1, t: -1, r: 101, b: 101 }).is_empty());
    }

    #[test]
    fn covered_parts_are_greyed() {
        let top = R { l: 50, t: 0, r: 100, b: 100 };
        let shared = R { l: 0, t: 0, r: 100, b: 100 };
        let shown = shown_parts(&[(top, false), (shared, true)], shared);
        assert_eq!(shown, vec![R { l: 0, t: 0, r: 50, b: 100 }]);
        let data = vec![200; 100 * 100 * 4];
        let out = crop_masked(&Pixels { w: 100, h: 100, stride: 400, data: &data }, shared, &shown);
        assert_eq!(&out.data[0..4], &[200, 200, 200, 200]);
        assert_eq!(&out.data[60 * 4..61 * 4], &HIDDEN);
    }
}
