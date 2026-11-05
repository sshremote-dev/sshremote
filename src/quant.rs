//! Per-image palettes for sixel, which allows at most 256 colours per image:
//! exact when the image has few colours (typical UI), median cut otherwise.

use std::collections::HashMap;

fn key(p: [u8; 3]) -> usize {
    (p[0] as usize >> 3) << 10 | (p[1] as usize >> 3) << 5 | p[2] as usize >> 3
}

/// Fills `pal` and `idx` (one entry per pixel) for the pixels `px`.
pub fn quantize(px: &[[u8; 3]], pal: &mut Vec<[u8; 3]>, idx: &mut Vec<u8>) {
    pal.clear();
    idx.clear();
    let mut exact: HashMap<[u8; 3], u8> = HashMap::new();
    for &p in px {
        if exact.len() > 256 {
            break;
        }
        let n = exact.len();
        exact.entry(p).or_insert(n as u8);
    }
    if exact.len() <= 256 {
        pal.resize(exact.len(), [0; 3]);
        for (&c, &i) in &exact {
            pal[i as usize] = c;
        }
        idx.extend(px.iter().map(|p| exact[p]));
        return;
    }

    let mut hist = vec![0u32; 1 << 15];
    for &p in px {
        hist[key(p)] += 1;
    }
    let mut bins: Vec<(usize, u32)> = hist.iter().enumerate().filter(|(_, n)| **n > 0).map(|(k, &n)| (k, n)).collect();
    let chan = |k: usize, c: usize| (k >> (10 - 5 * c)) & 31;
    // Boxes are ranges of `bins`; split the box with the widest channel range until 256.
    let mut boxes = vec![(0usize, bins.len())];
    while boxes.len() < 256 {
        let mut best = None;
        for (bi, &(s, e)) in boxes.iter().enumerate() {
            if e - s < 2 {
                continue;
            }
            for c in 0..3 {
                let (lo, hi) =
                    bins[s..e].iter().fold((31, 0), |(lo, hi), &(k, _)| (lo.min(chan(k, c)), hi.max(chan(k, c))));
                let pop: u64 = bins[s..e].iter().map(|b| b.1 as u64).sum();
                let score = (hi - lo) as u64 * pop;
                if best.is_none_or(|(_, _, sc)| score > sc) {
                    best = Some((bi, c, score));
                }
            }
        }
        let Some((bi, c, score)) = best else { break };
        if score == 0 {
            break;
        }
        let (s, e) = boxes[bi];
        bins[s..e].sort_unstable_by_key(|&(k, _)| chan(k, c));
        let total: u64 = bins[s..e].iter().map(|b| b.1 as u64).sum();
        let mut acc = 0u64;
        let mut cut = s + 1;
        for (i, b) in bins[s..e].iter().enumerate() {
            acc += b.1 as u64;
            if acc * 2 >= total {
                cut = (s + i + 1).clamp(s + 1, e - 1);
                break;
            }
        }
        boxes[bi] = (s, cut);
        boxes.push((cut, e));
    }
    let mut map = vec![0u8; 1 << 15];
    for (i, &(s, e)) in boxes.iter().enumerate() {
        let (mut sum, mut n) = ([0u64; 3], 0u64);
        for &(k, cnt) in &bins[s..e] {
            map[k] = i as u8;
            for (c, acc) in sum.iter_mut().enumerate() {
                *acc += ((chan(k, c) << 3) | 4) as u64 * cnt as u64;
            }
            n += cnt as u64;
        }
        pal.push(sum.map(|v| (v / n.max(1)) as u8));
    }
    idx.extend(px.iter().map(|&p| map[key(p)]));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn few_colours_are_exact() {
        let px = vec![[1, 2, 3], [200, 100, 50], [1, 2, 3]];
        let (mut pal, mut idx) = (Vec::new(), Vec::new());
        quantize(&px, &mut pal, &mut idx);
        let back: Vec<_> = idx.iter().map(|&i| pal[i as usize]).collect();
        assert_eq!(back, px);
    }

    #[test]
    fn many_colours_stay_close() {
        let px: Vec<[u8; 3]> = (0..4096u32).map(|i| [(i * 7) as u8, (i * 13) as u8, (i >> 4) as u8]).collect();
        let (mut pal, mut idx) = (Vec::new(), Vec::new());
        quantize(&px, &mut pal, &mut idx);
        assert!(pal.len() <= 256 && idx.len() == px.len());
        let err: f64 = px
            .iter()
            .zip(&idx)
            .map(|(p, &i)| (0..3).map(|c| (p[c] as f64 - pal[i as usize][c] as f64).abs()).sum::<f64>())
            .sum::<f64>()
            / px.len() as f64;
        assert!(err < 60.0, "mean error {err}");
    }
}
