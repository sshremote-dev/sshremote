//! A screen sent smaller than it is, for a viewer that shows it so: each pixel becomes the mean
//! of the screen's pixels it covers (area averaging), as the screen itself would look at that
//! size. Picking the nearest pixel instead drops whole strokes of text and breaks thin lines.
//! Rows whose pixels did not change since the last picture keep what they had, so typing or a
//! pointer moving costs a few rows, not the screen.

/// A whole pixel's weight along one axis, in fixed point.
const ONE: u32 = 1 << 12;

/// For each pixel along one axis of the smaller picture: the first source pixel it covers and
/// its weights, at most `n`, summing to [`ONE`].
#[derive(Default)]
struct Taps {
    first: Vec<usize>,
    len: Vec<usize>,
    n: usize,
    w: Vec<u32>,
}

impl Taps {
    fn new(src: usize, dst: usize) -> Taps {
        let n = src.div_ceil(dst) + 1;
        let (mut first, mut len, mut w) = (Vec::with_capacity(dst), Vec::with_capacity(dst), vec![0u32; dst * n]);
        for i in 0..dst {
            // In units of 1/dst of a source pixel, pixel i covers [i * src, (i + 1) * src).
            let (a, b) = (i * src, (i + 1) * src);
            let mut taps = Vec::with_capacity(n);
            let mut j = a / dst;
            while j < src && j * dst < b {
                let (lo, hi) = ((j * dst).max(a), ((j + 1) * dst).min(b));
                taps.push((j, ((hi - lo) as u64 * ONE as u64 / src as u64) as u32));
                j += 1;
            }
            // What rounding left goes to the largest share: a flat colour stays exactly itself.
            let sum: u32 = taps.iter().map(|t| t.1).sum();
            if let Some(m) = taps.iter_mut().max_by_key(|t| t.1) {
                m.1 += ONE - sum;
            }
            // Slivers too thin to weigh anything at an extreme shrink.
            while taps.len() > 1 && taps[0].1 == 0 {
                taps.remove(0);
            }
            while taps.len() > 1 && taps[taps.len() - 1].1 == 0 {
                taps.pop();
            }
            first.push(taps[0].0);
            len.push(taps.len());
            for (k, t) in taps.iter().enumerate() {
                w[i * n + k] = t.1;
            }
        }
        Taps { first, len, n, w }
    }

    /// Pixel `i`'s first source pixel and its weights.
    fn at(&self, i: usize) -> (usize, &[u32]) {
        (self.first[i], &self.w[i * self.n..i * self.n + self.len[i]])
    }
}

/// Shrinks pictures of one size to another, `N` bytes a pixel (4 as captured, 3 as a viewer
/// keeps them), keeping the last of each.
#[derive(Default)]
pub struct Shrink<const N: usize = 4> {
    size: (usize, usize, usize, usize),
    xs: Taps,
    ys: Taps,
    /// The last source picture, rows packed.
    prev: Vec<u8>,
    out: Vec<u8>,
    acc: Vec<u32>,
    changed: Vec<bool>,
    /// The rows of the last result drawn anew, first and past the last.
    pub drawn: Option<(usize, usize)>,
}

impl<const N: usize> Shrink<N> {
    /// `src` (`sw` x `sh`, rows `stride` bytes apart) at `w` x `h`, its rows packed.
    pub fn run(&mut self, src: &[u8], sw: usize, sh: usize, stride: usize, w: usize, h: usize) -> &[u8] {
        let (w, h) = (w.max(1), h.max(1));
        if self.size != (sw, sh, w, h) {
            self.size = (sw, sh, w, h);
            (self.xs, self.ys) = (Taps::new(sw, w), Taps::new(sh, h));
            self.prev.clear();
            self.out = vec![0; w * h * N];
            self.acc = vec![0; sw * N];
        }
        let row = sw * N;
        let line = |r: usize| &src[r * stride..r * stride + row];
        let fresh = self.prev.len() != row * sh;
        self.changed.clear();
        self.changed.extend((0..sh).map(|r| fresh || line(r) != &self.prev[r * row..(r + 1) * row]));
        let (xs, ys, changed) = (&self.xs, &self.ys, &self.changed);
        let dirty = |y: usize| {
            let (r0, wy) = ys.at(y);
            changed[r0..r0 + wy.len()].iter().any(|&c| c)
        };
        let draw = |y: usize, acc: &mut [u32], out: &mut [u8]| {
            // Down the rows it covers, then across.
            let (r0, wy) = ys.at(y);
            acc.fill(0);
            for (k, &wk) in wy.iter().enumerate() {
                for (a, &v) in acc.iter_mut().zip(line(r0 + k)) {
                    *a += wk * v as u32;
                }
            }
            for (x, px) in out.as_chunks_mut::<N>().0.iter_mut().enumerate() {
                let (c0, wx) = xs.at(x);
                let mut sum = [0u32; N];
                for (&wk, a) in wx.iter().zip(acc[c0 * N..].as_chunks::<N>().0) {
                    for c in 0..N {
                        // A column sum is at most 255 * ONE; a sixteenth of a level is kept.
                        sum[c] += wk * (a[c] >> 8);
                    }
                }
                for c in 0..N {
                    px[c] = ((sum[c] + (1 << 15)) >> 16) as u8;
                }
            }
        };
        let rows: Vec<usize> = (0..h).filter(|&y| dirty(y)).collect();
        self.drawn = rows.first().zip(rows.last()).map(|(a, b)| (*a, b + 1));
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(4);
        if rows.len() < 64 || threads < 2 {
            for &y in &rows {
                draw(y, &mut self.acc, &mut self.out[y * w * N..(y + 1) * w * N]);
            }
        } else {
            // Most of the screen changed: bands of rows on threads of their own.
            let band = h.div_ceil(threads);
            std::thread::scope(|scope| {
                for (i, out) in self.out.chunks_mut(band * w * N).enumerate() {
                    let (draw, dirty) = (&draw, &dirty);
                    scope.spawn(move || {
                        let mut acc = vec![0u32; row];
                        for (k, out) in out.chunks_exact_mut(w * N).enumerate() {
                            let y = i * band + k;
                            if dirty(y) {
                                draw(y, &mut acc, out);
                            }
                        }
                    });
                }
            });
        }
        if fresh {
            self.prev.clear();
            (0..sh).for_each(|r| self.prev.extend_from_slice(line(r)));
        } else {
            for r in (0..sh).filter(|&r| self.changed[r]) {
                self.prev[r * row..(r + 1) * row].copy_from_slice(line(r));
            }
        }
        &self.out
    }

}

impl<const N: usize> Shrink<N> {
    /// The last result.
    pub fn result(&self) -> &[u8] {
        &self.out
    }
}

impl Shrink<4> {
    /// [`Shrink::run`] as an [`super::Image`].
    pub fn image(&mut self, src: &[u8], sw: usize, sh: usize, stride: usize, w: usize, h: usize) -> super::Image {
        let data = self.run(src, sw, sh, stride, w, h).to_vec();
        super::Image { w: w.max(1), h: h.max(1), stride: w.max(1) * 4, data }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&f(x, y));
            }
        }
        v
    }

    #[test]
    fn a_flat_colour_stays_itself_and_halving_averages_four() {
        let src = picture(8, 6, |_, _| [10, 200, 255, 0]);
        let mut s = Shrink::<4>::default();
        assert!(s.run(&src, 8, 6, 32, 5, 4).chunks(4).all(|p| p == [10, 200, 255, 0]));
        let src = picture(4, 2, |x, y| [((x + y * 4) * 10) as u8, 0, 0, 0]);
        let out = s.run(&src, 4, 2, 16, 2, 1).to_vec();
        // (0 + 10 + 40 + 50) / 4 and (20 + 30 + 60 + 70) / 4.
        assert_eq!((out[0], out[4]), (25, 45));
    }

    /// A one-pixel line at two thirds of the size is still there, fainter, where picking the
    /// nearest pixel would drop one line in three.
    #[test]
    fn thin_lines_stay_in_sight() {
        let (w, h) = (300, 30);
        let src = picture(w, h, |x, _| if x % 3 == 1 { [0, 0, 0, 0] } else { [255, 255, 255, 0] });
        let mut s = Shrink::<4>::default();
        let out = s.run(&src, w, h, w * 4, 200, 20);
        let row: Vec<u8> = out[..200 * 4].chunks(4).map(|p| p[0]).collect();
        assert!(row.windows(2).all(|p| p[0].min(p[1]) < 200), "every line leaves a mark: {row:?}");
    }

    /// Rows that did not change keep what they had; a changed row is drawn again.
    #[test]
    fn only_changed_rows_are_drawn_again() {
        let (w, h) = (90, 60);
        let mut src = picture(w, h, |x, y| [(x * 2) as u8, (y * 4) as u8, 7, 0]);
        let mut s = Shrink::<4>::default();
        let first = s.run(&src, w, h, w * 4, 60, 40).to_vec();
        for x in 0..w {
            src[(30 * w + x) * 4] = 255;
        }
        let second = s.run(&src, w, h, w * 4, 60, 40).to_vec();
        let mut whole = Shrink::<4>::default();
        assert_eq!(second, whole.run(&src, w, h, w * 4, 60, 40), "the same as shrinking it anew");
        let differ: Vec<usize> = (0..40).filter(|&y| first[y * 240..(y + 1) * 240] != second[y * 240..(y + 1) * 240]).collect();
        assert_eq!(differ, vec![20], "only the row over the changed one");
    }

    /// A 1920x1080 screen at 1280x720 in a few milliseconds, and its unchanged picture faster.
    #[test]
    #[ignore]
    fn shrinking_a_screen_is_quick() {
        let (w, h) = (1920, 1080);
        let src = picture(w, h, |x, y| [(x ^ y) as u8, (x * 3) as u8, (y * 5) as u8, 0]);
        let mut s = Shrink::<4>::default();
        let t = std::time::Instant::now();
        s.run(&src, w, h, w * 4, 1280, 720);
        let whole = t.elapsed();
        let t = std::time::Instant::now();
        s.run(&src, w, h, w * 4, 1280, 720);
        let still = t.elapsed();
        eprintln!("whole {whole:?}, unchanged {still:?}");
        assert!(whole.as_millis() < 30 && still.as_millis() < 5);
    }
}
