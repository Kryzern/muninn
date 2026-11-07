//! The photo field: photos resting at different depths, never overlapping,
//! drifting a little; on a search the matches come forward into rows under
//! the bar while the rest step back. Everything eases exponentially (no
//! springs, so nothing overshoots), and a photo on the move is briefly out of
//! focus before it sharpens where it lands. The drift follows activity: it
//! settles to stillness a few seconds after the pointer stops, so a window
//! nobody is touching costs nothing to draw.

use crate::gfx::Rect;

/// Space kept between photos at rest; drift and parallax stay inside it.
const GAP: f32 = 22.0;
/// Most results laid out at once.
pub const MAX_RESULTS: usize = 48;

pub struct Item {
    /// Index of the photo in the index.
    pub id: usize,
    /// The photo's path hash, which survives the index changing.
    pub key: u64,
    /// Width / height.
    pub a: f32,
    hx: f32,
    hy: f32,
    hz: f32,
    ph: f32,
    sp: f32,
    x: f32,
    y: f32,
    pub z: f32,
    s: f32,
    bx: f32,
    by: f32,
    bs: f32,
    bz: f32,
    gx: f32,
    gy: f32,
    gs: f32,
    blur: f32,
    /// In the resting crowd (not only here because it was a result).
    idle: bool,
    pub matched: bool,
    /// On screen, in DIPs, after parallax.
    pub rect: Rect,
}

impl Item {
    /// Mip bias for the depth blur, plus a little while moving.
    pub fn blur(&self) -> f32 {
        (1.0 - self.z.clamp(0.0, 1.0)) * 4.4 + self.blur
    }
    pub fn gray(&self) -> f32 {
        ((1.0 - self.z.clamp(0.0, 1.0)) * 1.15).min(1.0)
    }
    pub fn dim(&self) -> f32 {
        0.14 + 0.86 * self.z.clamp(0.0, 1.0)
    }
    pub fn sharp(&self) -> bool {
        self.z > 0.85
    }
}

fn hash(i: usize, k: f32) -> f32 {
    let x = ((i as f32) * 12.9898 + k).sin() * 43758.547;
    x - x.floor()
}

pub struct Field {
    pub items: Vec<Item>,
    w: f32,
    h: f32,
    bw: f32,
    /// Pointer, -0.5..0.5, and its smoothed value.
    mx: f32,
    my: f32,
    smx: f32,
    smy: f32,
    /// Top of the results area (under the bar and its chips).
    top: f32,
    pub searching: bool,
    /// 1 normal, lower pushes the whole crowd back (a panel is open).
    pub depth: f32,
    /// How bright the resting crowd may get: 1, or lower for the quiet field.
    pub quiet: f32,
    spawned: usize,
    first: bool,
    /// 1 right after activity, easing to 0: scales the drift.
    wake: f32,
}

impl Field {
    pub fn new() -> Field {
        Field { items: vec![], w: 1.0, h: 1.0, bw: 160.0, mx: 0.0, my: 0.0, smx: 0.0, smy: 0.0, top: 140.0, searching: false, depth: 1.0, quiet: 1.0, spawned: 0, first: true, wake: 1.0 }
    }

    fn item(&mut self, id: usize, a: f32, key: u64, idle: bool) -> Item {
        let i = self.spawned;
        self.spawned += 1;
        // A golden-angle spiral spreads them out before relaxing removes overlaps.
        let n = 40.0;
        let r = 0.14 + (((i % 40) as f32 + 0.5) / n).sqrt() * 0.44;
        let ang = i as f32 * 2.39996;
        let hx = 0.5 + r * ang.cos();
        let hy = 0.5 + r * ang.sin() * 0.86;
        let hz = 0.2 + 0.8 * hash(i, 4.1);
        Item {
            id,
            key,
            a: a.clamp(0.4, 2.6),
            hx,
            hy,
            hz,
            ph: hash(i, 11.0) * 6.28,
            sp: 0.6 + hash(i, 7.0),
            x: hx * self.w,
            y: hy * self.h,
            z: 0.0,
            s: 0.3,
            bx: hx * self.w,
            by: hy * self.h,
            bs: 0.5,
            bz: 0.0,
            gx: 0.0,
            gy: 0.0,
            gs: 0.0,
            blur: 0.0,
            idle,
            matched: false,
            rect: Rect::default(),
        }
    }

    /// The resting crowd: (photo id, aspect) pairs. Keeps photos already here.
    pub fn set_idle(&mut self, photos: &[(usize, f32, u64)]) {
        for it in &mut self.items {
            it.idle = photos.iter().any(|p| p.2 == it.key);
        }
        for &(id, a, key) in photos {
            if !self.items.iter().any(|it| it.key == key) {
                let it = self.item(id, a, key, true);
                self.items.push(it);
            }
        }
        self.layout();
    }

    /// Matches for the current search, best first. Empty with `searching`
    /// false goes back to browsing.
    pub fn set_results(&mut self, hits: &[(usize, f32, u64)], searching: bool) {
        self.searching = searching;
        for it in &mut self.items {
            it.matched = false;
        }
        let mut order = vec![];
        self.wake = 1.0;
        for &(id, a, key) in hits.iter().take(MAX_RESULTS) {
            let k = match self.items.iter().position(|it| it.key == key) {
                Some(k) => k,
                None => {
                    // New arrivals come forward out of the blur, from the back.
                    let it = self.item(id, a, key, false);
                    self.items.push(it);
                    self.items.len() - 1
                }
            };
            self.items[k].matched = true;
            order.push(k);
        }
        self.grid(&order);
        self.layout();
    }

    pub fn resize(&mut self, w: f32, h: f32, top: f32) {
        let changed = (w - self.w).abs() > 0.5 || (h - self.h).abs() > 0.5 || (top - self.top).abs() > 0.5;
        self.w = w.max(1.0);
        self.h = h.max(1.0);
        self.top = top;
        self.bw = (w / 7.0).clamp(110.0, 200.0);
        if changed {
            let order: Vec<usize> = self.result_order();
            self.grid(&order);
            self.layout();
        }
    }

    fn result_order(&self) -> Vec<usize> {
        let mut o: Vec<usize> = (0..self.items.len()).filter(|&k| self.items[k].matched).collect();
        o.sort_by(|a, b| self.items[*a].gy.total_cmp(&self.items[*b].gy).then(self.items[*a].gx.total_cmp(&self.items[*b].gx)));
        o
    }

    /// The index changed: point every photo at its new entry, drop the gone.
    pub fn remap(&mut self, now: &std::collections::HashMap<u64, usize>) {
        self.items.retain_mut(|it| match now.get(&it.key) {
            Some(&id) => {
                it.id = id;
                true
            }
            None => false,
        });
    }

    /// Something happened: let the field drift again for a while.
    pub fn wake(&mut self) {
        self.wake = 1.0;
    }

    pub fn pointer(&mut self, x: f32, y: f32) {
        self.wake = 1.0;
        self.mx = x / self.w - 0.5;
        self.my = y / self.h - 0.5;
    }

    pub fn pointer_left(&mut self) {
        self.mx = 0.0;
        self.my = 0.0;
    }

    /// The light for the glass leans toward the pointer.
    pub fn light(&self) -> (f32, f32) {
        let (lx, ly) = (-0.45 + self.smx * 0.9, -0.9 + self.smy * 0.3);
        let l = (lx * lx + ly * ly).sqrt();
        (lx / l, ly / l)
    }

    /// Results in justified rows from `top` down, shrunk until they all fit.
    fn grid(&mut self, order: &[usize]) {
        if order.is_empty() {
            return;
        }
        let (top, bottom) = (self.top, self.h - 28.0);
        let max_w = (self.w - 48.0).min(1240.0);
        let left = (self.w - max_w) / 2.0;
        let gap = 12.0;
        let avail = (bottom - top).max(60.0);
        let mut target = avail.min(260.0);
        let mut rows: Vec<(Vec<usize>, f32, bool)>;
        loop {
            rows = vec![];
            let (mut row, mut sum) = (vec![], 0.0);
            for &k in order {
                row.push(k);
                sum += self.items[k].a;
                if sum * target + gap * (row.len() - 1) as f32 >= max_w {
                    let h = (max_w - gap * (row.len() - 1) as f32) / sum;
                    rows.push((std::mem::take(&mut row), h, false));
                    sum = 0.0;
                }
            }
            if !row.is_empty() {
                let h = target.min((max_w - gap * (row.len() - 1) as f32) / sum);
                rows.push((row, h, true));
            }
            let total: f32 = rows.iter().map(|r| r.1).sum::<f32>() + gap * (rows.len() - 1) as f32;
            if total <= avail || target < 60.0 {
                break;
            }
            target *= 0.9;
        }
        let mut y = top;
        for (row, h, last) in rows {
            let w: f32 = row.iter().map(|&k| self.items[k].a * h).sum::<f32>() + gap * (row.len() - 1) as f32;
            let mut x = if last { (self.w - w) / 2.0 } else { left };
            for k in row {
                let it = &mut self.items[k];
                let tw = it.a * h;
                it.gx = x + tw / 2.0;
                it.gy = y + h / 2.0;
                it.gs = tw / self.bw;
                x += tw + gap;
            }
            y += h + gap;
        }
    }

    /// Resting places for the current mode, pushed apart so nothing overlaps.
    fn layout(&mut self) {
        let results = self.items.iter().any(|it| it.matched);
        let (w, h, bw) = (self.w, self.h, self.bw);
        for it in &mut self.items {
            if it.matched {
                (it.bx, it.by, it.bs, it.bz) = (it.gx, it.gy, it.gs, 1.0);
            } else if results || !it.idle {
                let (dx, dy) = (it.hx - 0.5, it.hy - 0.5);
                it.bx = (0.5 + dx * 1.45) * w;
                it.by = (0.5 + dy * 1.45) * h;
                it.bs = 0.3 + 0.28 * it.hz;
                // a former result that isn't part of the crowd fades away
                it.bz = if it.idle { 0.04 } else { 0.0 };
            } else {
                it.bx = it.hx * w;
                it.by = it.hy * h;
                it.bs = 0.45 + 0.7 * it.hz;
                it.bz = if self.searching { it.hz * 0.35 } else { it.hz };
            }
        }
        let fixed: Vec<usize> = (0..self.items.len()).filter(|&k| self.items[k].matched).collect();
        let free: Vec<usize> = (0..self.items.len()).filter(|&k| !self.items[k].matched && self.items[k].bz > 0.0).collect();
        let wh = |it: &Item| {
            let w = bw * it.bs;
            (w, w / it.a)
        };
        for _ in 0..120 {
            let mut moved = false;
            for i in 0..free.len() {
                for j in (i + 1..free.len()).map(|j| free[j]).chain(fixed.iter().copied()) {
                    let a = free[i];
                    let (aw, ah) = wh(&self.items[a]);
                    let (bw2, bh) = wh(&self.items[j]);
                    let dx = self.items[j].bx - self.items[a].bx;
                    let dy = self.items[j].by - self.items[a].by;
                    let ox = (aw + bw2) / 2.0 + GAP - dx.abs();
                    let oy = (ah + bh) / 2.0 + GAP - dy.abs();
                    if ox <= 0.0 || oy <= 0.0 {
                        continue;
                    }
                    moved = true;
                    let pinned = self.items[j].matched;
                    let share = if pinned { 1.0 } else { 0.5 };
                    let tie = if self.items[a].ph < self.items[j].ph { -1.0 } else { 1.0 };
                    if ox < oy {
                        let s = if dx != 0.0 { dx.signum() } else { tie } * ox;
                        self.items[a].bx -= s * share;
                        if !pinned {
                            self.items[j].bx += s * 0.5;
                        }
                    } else {
                        let s = if dy != 0.0 { dy.signum() } else { tie } * oy;
                        self.items[a].by -= s * share;
                        if !pinned {
                            self.items[j].by += s * 0.5;
                        }
                    }
                }
            }
            if !moved {
                break;
            }
        }
        if self.first {
            for it in &mut self.items {
                it.x = it.bx;
                it.y = it.by;
            }
            self.first = self.items.is_empty();
        }
    }

    /// Advance by `dt` ms. Returns whether anything is still moving.
    pub fn step(&mut self, now_ms: f64, dt: f32, animate: bool) -> bool {
        let ease = |tau: f32| if animate { 1.0 - (-dt / tau).exp() } else { 1.0 };
        let (kp, kz, km) = (ease(240.0), ease(300.0), ease(450.0));
        self.smx += (self.mx - self.smx) * km;
        self.smy += (self.my - self.smy) * km;
        let results = self.items.iter().any(|it| it.matched);
        let slow = (now_ms * 0.001) as f32;
        self.wake = if animate { (self.wake - dt / 3000.0).max(0.0) } else { 0.0 };
        // eased, so the drift fades out instead of stopping dead
        let w = self.wake * self.wake * (3.0 - 2.0 * self.wake);
        let amp = if results { 2.0 } else { GAP * 0.3 } * w;
        let mut moving = w > 0.0 || (self.mx - self.smx).abs() + (self.my - self.smy).abs() > 0.0005;
        for it in &mut self.items {
            let (tx, ty) = if animate && w > 0.0 {
                (it.bx + (slow * 0.22 * it.sp + it.ph).sin() * amp, it.by + (slow * 0.18 * it.sp + it.ph).cos() * amp)
            } else {
                (it.bx, it.by)
            };
            let tz = if it.matched { it.bz } else { it.bz * self.depth * self.quiet };
            let tz = if it.matched { tz * self.depth.max(0.25) } else { tz };
            let (ox, oy) = (it.x, it.y);
            it.x += (tx - it.x) * kp;
            it.y += (ty - it.y) * kp;
            it.s += (it.bs - it.s) * kp;
            it.z += (tz - it.z) * kz;
            let speed = ((it.x - ox).powi(2) + (it.y - oy).powi(2)).sqrt() / dt.max(1.0);
            it.blur = (speed * 1.6).min(2.6);
            if (tz - it.z).abs() > 0.003 || (it.bs - it.s).abs() > 0.002 || speed > 0.004 {
                moving = true;
            }
            let par = (it.z - 0.45) * if results { 4.0 } else { 20.0 };
            let (cw, ch) = (self.bw * it.s, self.bw / it.a * it.s);
            it.rect = Rect::new(it.x + self.smx * par - cw / 2.0, it.y + self.smy * par - ch / 2.0, cw, ch);
        }
        // Photos that have faded out entirely are dropped.
        self.items.retain(|it| it.idle || it.matched || it.z > 0.01);
        moving
    }

    /// Back to front, for drawing.
    pub fn draw_order(&self) -> Vec<&Item> {
        let mut v: Vec<&Item> = self.items.iter().collect();
        v.sort_by(|a, b| a.z.total_cmp(&b.z));
        v
    }

    /// The front-most photo under a point.
    pub fn hit(&self, x: f32, y: f32) -> Option<&Item> {
        self.draw_order().into_iter().rev().find(|it| it.z > 0.05 && it.rect.contains(x, y))
    }

    pub fn find(&self, id: usize) -> Option<&Item> {
        self.items.iter().find(|it| it.id == id)
    }

    /// Results in reading order.
    pub fn results(&self) -> Vec<usize> {
        self.result_order().into_iter().map(|k| self.items[k].id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlaps(f: &Field) -> usize {
        let r: Vec<_> = f.items.iter().filter(|it| it.bz > 0.0 || it.matched).map(|it| {
            let w = f.bw * it.bs;
            Rect::new(it.bx - w / 2.0, it.by - w / it.a / 2.0, w, w / it.a)
        }).collect();
        let mut n = 0;
        for i in 0..r.len() {
            for j in i + 1..r.len() {
                let (a, b) = (r[i], r[j]);
                if a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h {
                    n += 1;
                }
            }
        }
        n
    }

    #[test]
    fn crowd_and_results_do_not_overlap() {
        let mut f = Field::new();
        f.resize(1440.0, 900.0, 140.0);
        let photos: Vec<(usize, f32, u64)> = (0..30).map(|i| (i, [1.5, 1.0, 0.8][i % 3], i as u64)).collect();
        f.set_idle(&photos);
        assert_eq!(overlaps(&f), 0);
        f.set_results(&[(3, 1.5, 3), (7, 0.8, 7), (40, 1.0, 40), (41, 1.5, 41)], true);
        assert_eq!(overlaps(&f), 0);
        assert_eq!(f.items.iter().filter(|it| it.matched).count(), 4);
        for _ in 0..200 {
            f.step(0.0, 16.0, true);
        }
        let r = f.find(40).unwrap();
        assert!(r.sharp() && r.rect.y >= 139.0);
    }

    #[test]
    fn leaving_results_fade_and_go() {
        let mut f = Field::new();
        f.resize(1200.0, 800.0, 140.0);
        f.set_idle(&[(1, 1.5, 1), (2, 1.5, 2)]);
        f.set_results(&[(99, 1.0, 99)], true);
        for _ in 0..60 {
            f.step(0.0, 16.0, true);
        }
        f.set_results(&[], false);
        for _ in 0..400 {
            f.step(0.0, 16.0, true);
        }
        assert!(f.find(99).is_none());
        assert_eq!(f.items.len(), 2);
    }

    #[test]
    fn goes_still_when_left_alone() {
        let mut f = Field::new();
        f.resize(1200.0, 800.0, 140.0);
        f.set_idle(&[(1, 1.5, 1), (2, 1.0, 2), (3, 0.8, 3)]);
        let mut t = 0.0;
        let mut moving = true;
        for _ in 0..600 {
            t += 16.0;
            moving = f.step(t, 16.0, true);
        }
        assert!(!moving, "still drawing after 10 s with nothing happening");
        f.pointer(100.0, 100.0);
        assert!(f.step(t + 16.0, 16.0, true));
    }
}
