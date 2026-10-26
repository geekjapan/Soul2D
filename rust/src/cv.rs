use crate::render::MAX_TEXTURE_PIXELS;
use serde_json::{json, Value};
use std::collections::HashSet;

const QUERY_MAX_PIXELS: u64 = 65_536;
const QUERY_BUDGET: u64 = 100_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DistanceSample {
    pub distance: f32,
    pub nearest: Point,
}

#[derive(Debug)]
pub struct DistanceField {
    width: u32,
    height: u32,
    samples: Vec<DistanceSample>,
}

impl DistanceField {
    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn samples(&self) -> &[DistanceSample] {
        &self.samples
    }
}

#[derive(Debug)]
pub enum DistanceTransform {
    NoBackground,
    Field(DistanceField),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetrievalMode {
    External,
    List,
    Ccomp,
    Tree,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApproximationMethod {
    None,
    Simple,
    Tc89L1,
    Tc89Kcos,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hierarchy {
    pub next: Option<usize>,
    pub prev: Option<usize>,
    pub child: Option<usize>,
    pub parent: Option<usize>,
}

#[derive(Debug)]
pub struct Contours {
    pub points: Vec<Vec<Point>>,
    pub hierarchy: Vec<Hierarchy>,
}

pub struct WorkBudget {
    remaining: u64,
}

impl WorkBudget {
    pub fn new(visits: u64) -> Self {
        Self { remaining: visits }
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub(crate) fn charge(&mut self, visits: u64) -> Result<(), String> {
        if self.remaining < visits {
            return Err("work budget exhausted".to_owned());
        }
        self.remaining -= visits;
        Ok(())
    }
}

#[derive(Debug)]
pub struct BinaryMask {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl BinaryMask {
    fn checked_size(width: u32, height: u32) -> Result<u64, String> {
        if width == 0 || height == 0 {
            return Err("mask dimensions must be nonzero".to_owned());
        }
        let total = (width as u64)
            .checked_mul(height as u64)
            .ok_or("mask size overflows")?;
        if total > MAX_TEXTURE_PIXELS {
            return Err(format!("mask pixel count {total} exceeds limit {MAX_TEXTURE_PIXELS}"));
        }
        Ok(total)
    }

    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let total = Self::checked_size(width, height)?;
        if pixels.len() as u64 != total {
            return Err(format!(
                "mask pixel count {} does not match size {width}x{height}",
                pixels.len()
            ));
        }
        Ok(Self { width, height, pixels })
    }

    pub fn from_rgba(width: u32, height: u32, rgba: &[u8], threshold: u8) -> Result<Self, String> {
        let total = Self::checked_size(width, height)?;
        let expected = total
            .checked_mul(4)
            .ok_or("mask size overflows")?;
        if rgba.len() as u64 != expected {
            return Err(format!(
                "rgba length {} does not match {width}x{height}x4",
                rgba.len()
            ));
        }
        let pixels = rgba
            .chunks_exact(4)
            .map(|channel| u8::from(channel[3] >= threshold) * 255)
            .collect();
        Self::new(width, height, pixels)
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    fn at(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && (x as u32) < self.width
            && (y as u32) < self.height
            && self.pixels[y as usize * self.width as usize + x as usize] != 0
    }

    fn total(&self) -> usize {
        self.pixels.len()
    }

    pub fn distance_transform(
        &self,
        budget: &mut WorkBudget,
    ) -> Result<DistanceTransform, String> {
        let width = self.width as usize;
        let height = self.height as usize;
        let total = self.total();
        budget.charge(total as u64)?;
        if !self.pixels.contains(&0) {
            return Ok(DistanceTransform::NoBackground);
        }
        let mut farr = vec![0.0f32; total];
        for (index, pixel) in self.pixels.iter().enumerate() {
            if *pixel != 0 {
                farr[index] = f32::MAX;
            }
        }
        let mut garr = vec![0.0f32; total];
        let mut row_nearest = vec![0i32; total];
        for y in 0..height {
            let row = &farr[y * width..(y + 1) * width];
            let (drow, ind) = dt1d(row, budget)?;
            garr[y * width..(y + 1) * width].copy_from_slice(&drow);
            row_nearest[y * width..(y + 1) * width].copy_from_slice(&ind);
        }
        let mut dtarr = vec![0.0f32; total];
        let mut nearest = vec![0i32; total * 2];
        for x in 0..width {
            let mut col = vec![0.0f32; height];
            for y in 0..height {
                col[y] = garr[y * width + x];
            }
            let (dcol, col_ind) = dt1d(&col, budget)?;
            for y in 0..height {
                dtarr[y * width + x] = dcol[y];
                let r = col_ind[y];
                nearest[(y * width + x) * 2] = row_nearest[r as usize * width + x];
                nearest[(y * width + x) * 2 + 1] = r;
            }
        }
        let mut samples = Vec::with_capacity(total);
        for index in 0..total {
            let distance = if dtarr[index] != f32::MAX {
                dtarr[index].sqrt()
            } else {
                0.0
            };
            samples.push(DistanceSample {
                distance,
                nearest: Point {
                    x: nearest[index * 2] as u32,
                    y: nearest[index * 2 + 1] as u32,
                },
            });
        }
        Ok(DistanceTransform::Field(DistanceField {
            width: self.width,
            height: self.height,
            samples,
        }))
    }

    pub fn thin(&self, budget: &mut WorkBudget) -> Result<BinaryMask, String> {
        let width = self.width as usize;
        let height = self.height as usize;
        budget.charge(self.total() as u64)?;
        let mut image = self.pixels.clone();
        let fg = |im: &[u8], x: i32, y: i32| -> bool {
            im[y as usize * width + x as usize] != 0
        };
        let count_neighbors = |im: &[u8], x: i32, y: i32| -> i32 {
            let mut count = 0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dy == 0 && dx == 0 {
                        continue;
                    }
                    if fg(im, x + dx, y + dy) {
                        count += 1;
                    }
                }
            }
            count
        };
        let count_transitions = |im: &[u8], x: i32, y: i32| -> i32 {
            let neighbors = [
                fg(im, x, y - 1),
                fg(im, x + 1, y - 1),
                fg(im, x + 1, y),
                fg(im, x + 1, y + 1),
                fg(im, x, y + 1),
                fg(im, x - 1, y + 1),
                fg(im, x - 1, y),
                fg(im, x - 1, y - 1),
            ];
            let mut transitions = 0;
            for k in 0..8 {
                if !neighbors[k] && neighbors[(k + 1) % 8] {
                    transitions += 1;
                }
            }
            transitions
        };
        let mut candidates: Vec<(i32, i32)> = Vec::new();
        for y in 1..height.saturating_sub(1) as i32 {
            for x in 1..width.saturating_sub(1) as i32 {
                budget.charge(1)?;
                if fg(&image, x, y) {
                    candidates.push((x, y));
                }
            }
        }
        let mut changed = true;
        while changed {
            changed = false;
            let mut to_delete = Vec::new();
            for &(x, y) in &candidates {
                budget.charge(1)?;
                if !fg(&image, x, y) {
                    continue;
                }
                let bp = count_neighbors(&image, x, y);
                if !(2..=6).contains(&bp) {
                    continue;
                }
                if count_transitions(&image, x, y) != 1 {
                    continue;
                }
                if fg(&image, x, y - 1) && fg(&image, x + 1, y) && fg(&image, x, y + 1) {
                    continue;
                }
                if fg(&image, x + 1, y) && fg(&image, x, y + 1) && fg(&image, x - 1, y) {
                    continue;
                }
                to_delete.push((x, y));
            }
            let mut next_candidates = Vec::new();
            if !to_delete.is_empty() {
                changed = true;
                for (x, y) in to_delete {
                    image[y as usize * width + x as usize] = 0;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let ny = y + dy;
                            let nx = x + dx;
                            if ny < 1 || ny >= height as i32 - 1 || nx < 1 || nx >= width as i32 - 1 {
                                continue;
                            }
                            next_candidates.push((nx, ny));
                        }
                    }
                }
                next_candidates.sort_by_key(|&(x, y)| (y, x));
                next_candidates.dedup();
            }
            let mut to_delete2 = Vec::new();
            for &(x, y) in &next_candidates {
                budget.charge(1)?;
                if !fg(&image, x, y) {
                    continue;
                }
                let bp = count_neighbors(&image, x, y);
                if !(2..=6).contains(&bp) {
                    continue;
                }
                if count_transitions(&image, x, y) != 1 {
                    continue;
                }
                if fg(&image, x, y - 1) && fg(&image, x + 1, y) && fg(&image, x - 1, y) {
                    continue;
                }
                if fg(&image, x, y - 1) && fg(&image, x, y + 1) && fg(&image, x - 1, y) {
                    continue;
                }
                to_delete2.push((x, y));
            }
            let mut next_candidates2 = Vec::new();
            if !to_delete2.is_empty() {
                changed = true;
                for (x, y) in to_delete2 {
                    image[y as usize * width + x as usize] = 0;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let ny = y + dy;
                            let nx = x + dx;
                            if ny < 1 || ny >= height as i32 - 1 || nx < 1 || nx >= width as i32 - 1 {
                                continue;
                            }
                            next_candidates2.push((nx, ny));
                        }
                    }
                }
                next_candidates2.sort_by_key(|&(x, y)| (y, x));
                next_candidates2.dedup();
            }
            candidates = next_candidates2;
            if candidates.is_empty() {
                for y in 1..height.saturating_sub(1) as i32 {
                    for x in 1..width.saturating_sub(1) as i32 {
                        budget.charge(1)?;
                        if fg(&image, x, y) {
                            candidates.push((x, y));
                        }
                    }
                }
                if candidates.is_empty() {
                    break;
                }
            }
        }
        BinaryMask::new(self.width, self.height, image)
    }

    pub fn longest_path(&self, budget: &mut WorkBudget) -> Result<Vec<Point>, String> {
        let width = self.width as usize;
        let height = self.height as usize;
        let total = self.total();
        budget.charge(total as u64)?;
        let mut visited = vec![false; total];
        let mut dist = vec![-1i32; total];
        let mut pred = vec![0usize; total];
        let mut longest: Vec<Point> = Vec::new();
        let neighbors = |point: usize,
                         budget: &mut WorkBudget|
         -> Result<Vec<usize>, String> {
            budget.charge(8)?;
            let cx = (point % width) as i32;
            let cy = (point / width) as i32;
            let mut out = Vec::with_capacity(8);
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = cx + dx;
                    let ny = cy + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let next = ny as usize * width + nx as usize;
                    if self.pixels[next] != 0 {
                        out.push(next);
                    }
                }
            }
            Ok(out)
        };
        for index in 0..total {
            budget.charge(1)?;
            if self.pixels[index] == 0 || visited[index] {
                continue;
            }
            let mut comp: Vec<usize> = Vec::new();
            let mut queue: Vec<usize> = vec![index];
            visited[index] = true;
            let mut head = 0;
            while head < queue.len() {
                let cur = queue[head];
                head += 1;
                comp.push(cur);
                for next in neighbors(cur, budget)? {
                    if !visited[next] {
                        visited[next] = true;
                        queue.push(next);
                    }
                }
            }
            let mut endpoints = Vec::new();
            for &point in &comp {
                budget.charge(1)?;
                if neighbors(point, budget)?.len() == 1 {
                    endpoints.push(point);
                }
            }
            let seed = endpoints.first().copied().unwrap_or(comp[0]);
            let mut bfs_diameter = |src: usize,
                                    budget: &mut WorkBudget|
             -> Result<(usize, Vec<Point>), String> {
                for &point in &comp {
                    budget.charge(1)?;
                    dist[point] = -1;
                }
                let mut queue: Vec<usize> = vec![src];
                dist[src] = 0;
                let mut head = 0;
                while head < queue.len() {
                    let cur = queue[head];
                    head += 1;
                    for next in neighbors(cur, budget)? {
                        if dist[next] == -1 {
                            dist[next] = dist[cur] + 1;
                            pred[next] = cur;
                            queue.push(next);
                        }
                    }
                }
                let mut farthest = src;
                let mut max_dist = 0;
                for &point in &comp {
                    budget.charge(1)?;
                    if dist[point] > max_dist {
                        max_dist = dist[point];
                        farthest = point;
                    }
                }
                let mut path = Vec::new();
                let mut cur = farthest;
                for _ in 0..=comp.len() {
                    budget.charge(1)?;
                    path.push(cur);
                    if cur == src {
                        break;
                    }
                    cur = pred[cur];
                }
                if *path.last().unwrap() != src {
                    return Err("path extraction lost its predecessor chain".to_owned());
                }
                path.reverse();
                Ok((
                    farthest,
                    path.iter()
                        .map(|&index| Point {
                            x: (index % width) as u32,
                            y: (index / width) as u32,
                        })
                        .collect(),
                ))
            };
            let (far, _) = bfs_diameter(seed, budget)?;
            let (_, path) = bfs_diameter(far, budget)?;
            if path.len() > longest.len() {
                longest = path;
            }
        }
        Ok(longest)
    }

    pub fn contours(
        &self,
        mode: RetrievalMode,
        method: ApproximationMethod,
        budget: &mut WorkBudget,
    ) -> Result<Contours, String> {
        let width = self.width as usize;
        let height = self.height as usize;
        let total = self.total();
        budget.charge(total as u64)?;
        let mut labels = vec![0i32; total];
        let mut label = 1i32;
        let mut found: Vec<Vec<Point>> = Vec::new();
        for y in 0..height as i32 {
            for x in 0..width as i32 {
                budget.charge(1)?;
                let index = y as usize * width + x as usize;
                if self.pixels[index] != 0 && labels[index] == 0 {
                    let left = if x > 0 {
                        self.pixels[y as usize * width + (x - 1) as usize]
                    } else {
                        0
                    };
                    if left == 0 {
                        let contour =
                            self.suzuki_abe(&mut labels, label, x, y, budget)?;
                        fill_interior(&mut labels, &contour, width, height, budget)?;
                        found.push(approximate(&contour, method, budget)?);
                        label += 1;
                    } else {
                        labels[index] = -1;
                    }
                }
            }
        }
        let hierarchy = if matches!(mode, RetrievalMode::Tree | RetrievalMode::Ccomp) {
            build_hierarchy(&found, budget)?
        } else {
            vec![
                Hierarchy {
                    next: None,
                    prev: None,
                    child: None,
                    parent: None,
                };
                found.len()
            ]
        };
        if mode == RetrievalMode::External {
            let keep: Vec<usize> = (0..found.len())
                .filter(|&index| hierarchy[index].parent.is_none())
                .collect();
            let mut contours = Vec::with_capacity(keep.len());
            let mut entries = Vec::with_capacity(keep.len());
            for index in keep {
                contours.push(found[index].clone());
                entries.push(hierarchy[index]);
            }
            return Ok(Contours { points: contours, hierarchy: entries });
        }
        Ok(Contours { points: found, hierarchy })
    }

    fn suzuki_abe(
        &self,
        labels: &mut [i32],
        label: i32,
        start_x: i32,
        start_y: i32,
        budget: &mut WorkBudget,
    ) -> Result<Vec<(i32, i32)>, String> {
        const DIRECTIONS: [[i32; 2]; 8] = [
            [0, 1],
            [-1, 1],
            [-1, 0],
            [-1, -1],
            [0, -1],
            [1, -1],
            [1, 0],
            [1, 1],
        ];
        const DIR_MAP: [[i32; 3]; 3] = [
            [3, 4, 5],
            [2, -1, 6],
            [1, 0, 7],
        ];
        let width = self.width as usize;
        let b = (start_x, start_y);
        let mut c = b;
        let mut p = (start_x, start_y - 1);
        let mut contour = vec![b];
        labels[start_y as usize * width + start_x as usize] = label;
        let mut first_iteration = true;
        let mut states: HashSet<(i32, i32, i32, i32)> = HashSet::new();
        loop {
            budget.charge(1)?;
            if !states.insert((c.0, c.1, p.0, p.1)) {
                return Err("contour trace revisited a directed state".to_owned());
            }
            let dx = p.0 - c.0;
            let dy = p.1 - c.1;
            let mut start_dir = 0usize;
            if (-1..=1).contains(&dx) && (-1..=1).contains(&dy) {
                let table = DIR_MAP[(dy + 1) as usize][(dx + 1) as usize];
                if table != -1 {
                    start_dir = ((table + 1) % 8) as usize;
                }
            }
            let mut found = None;
            for i in 0..8 {
                budget.charge(1)?;
                let idx = (start_dir + i) % 8;
                let nx = c.0 + DIRECTIONS[idx][0];
                let ny = c.1 + DIRECTIONS[idx][1];
                if !self.at(nx, ny) {
                    continue;
                }
                let label_at = labels[ny as usize * width + nx as usize];
                if label_at == 0 || label_at == label || ((nx, ny) == b && !first_iteration) {
                    found = Some(((nx, ny), idx));
                    break;
                }
            }
            let Some((next, next_dir)) = found else {
                break;
            };
            let offset = DIRECTIONS[(next_dir + 7) % 8];
            p = (c.0 + offset[0], c.1 + offset[1]);
            c = next;
            if c != b {
                labels[c.1 as usize * width + c.0 as usize] = label;
            }
            contour.push(c);
            if !first_iteration && c == b {
                break;
            }
            first_iteration = false;
        }
        Ok(contour)
    }
}

fn dt1d(f: &[f32], budget: &mut WorkBudget) -> Result<(Vec<f32>, Vec<i32>), String> {
    // The parabola intersections are computed in f64: squares above 2^24 (x > 4096) are not exact in f32.
    let n = f.len();
    let mut d = vec![0.0f32; n];
    let mut ind = vec![0i32; n];
    let mut v = vec![0i32; n];
    let mut z = vec![0.0f64; n + 1];
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    let mut k = 0usize;
    for q in 1..n {
        budget.charge(1)?;
        loop {
            let vk = f64::from(v[k]);
            let qf = q as f64;
            let s = ((f64::from(f[q]) + qf * qf) - (f64::from(f[v[k] as usize]) + vk * vk)) / (2.0 * (qf - vk));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            k += 1;
            v[k] = q as i32;
            z[k] = s;
            z[k + 1] = f64::INFINITY;
            break;
        }
    }
    k = 0;
    for q in 0..n {
        budget.charge(1)?;
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let vk = v[k] as usize;
        let dq = q as f64 - vk as f64;
        d[q] = (dq * dq + f64::from(f[vk])) as f32;
        ind[q] = v[k];
    }
    Ok((d, ind))
}

fn fill_interior(
    labels: &mut [i32],
    contour: &[(i32, i32)],
    width: usize,
    height: usize,
    budget: &mut WorkBudget,
) -> Result<(), String> {
    let mut min_x = contour[0].0;
    let mut max_x = contour[0].0;
    let mut min_y = contour[0].1;
    let mut max_y = contour[0].1;
    for &(x, y) in contour {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    for y in min_y..=max_y {
        let mut inters: Vec<f64> = Vec::new();
        for index in 0..contour.len() {
            budget.charge(1)?;
            let a = contour[index];
            let b = contour[(index + 1) % contour.len()];
            if (a.1 <= y && b.1 > y) || (a.1 > y && b.1 <= y) {
                let at_x =
                    a.0 as f64 + ((y - a.1) as f64 / (b.1 - a.1) as f64) * (b.0 - a.0) as f64;
                inters.push(at_x);
            }
        }
        if inters.len() >= 2 {
            inters.sort_by(f64::total_cmp);
            let mut index = 0;
            while index + 1 < inters.len() {
                let start = inters[index].ceil() as i32;
                let end = inters[index + 1].floor() as i32;
                for x in start..=end {
                    budget.charge(1)?;
                    if x >= 0 && x < width as i32 && y >= 0 && y < height as i32 {
                        let at = y as usize * width + x as usize;
                        if labels[at] == 0 {
                            labels[at] = -1;
                        }
                    }
                }
                index += 2;
            }
        }
    }
    Ok(())
}

fn point_in_polygon(
    p: (i32, i32),
    polygon: &[Point],
    budget: &mut WorkBudget,
) -> Result<bool, String> {
    let mut count = 0;
    for index in 0..polygon.len() {
        budget.charge(1)?;
        let a = polygon[index];
        let b = polygon[(index + 1) % polygon.len()];
        if (a.y as i32 > p.1) != (b.y as i32 > p.1) {
            let at_x = a.x as f64
                + ((p.1 - a.y as i32) as f64 / (b.y as i32 - a.y as i32) as f64)
                    * (b.x as i32 - a.x as i32) as f64;
            if (p.0 as f64) < at_x {
                count += 1;
            }
        }
    }
    Ok(count % 2 == 1)
}

fn build_hierarchy(
    contours: &[Vec<Point>],
    budget: &mut WorkBudget,
) -> Result<Vec<Hierarchy>, String> {
    let n = contours.len();
    let mut hierarchy = vec![
        Hierarchy {
            next: None,
            prev: None,
            child: None,
            parent: None,
        };
        n
    ];
    let mut boxes = Vec::with_capacity(n);
    for contour in contours {
        let mut min_x = contour[0].x as i32;
        let mut max_x = min_x;
        let mut min_y = contour[0].y as i32;
        let mut max_y = min_y;
        for point in contour {
            budget.charge(1)?;
            min_x = min_x.min(point.x as i32);
            max_x = max_x.max(point.x as i32);
            min_y = min_y.min(point.y as i32);
            max_y = max_y.max(point.y as i32);
        }
        boxes.push((min_x, min_y, max_x, max_y));
    }
    for i in 0..n {
        let rep = contours[i][0];
        let mut parent = None;
        for j in 0..n {
            if i == j {
                continue;
            }
            budget.charge(1)?;
            let (min_x, min_y, max_x, max_y) = boxes[j];
            let (rx, ry) = (rep.x as i32, rep.y as i32);
            if rx < min_x || rx > max_x || ry < min_y || ry > max_y {
                continue;
            }
            if point_in_polygon((rx, ry), &contours[j], budget)? {
                if parent.is_none_or(|p: usize| contours[j].len() < contours[p].len()) {
                    parent = Some(j);
                }
            }
        }
        hierarchy[i].parent = parent;
        if let Some(parent) = parent {
            if hierarchy[parent].child.is_none() {
                hierarchy[parent].child = Some(i);
            } else {
                let mut sibling = hierarchy[parent].child.unwrap();
                while let Some(next) = hierarchy[sibling].next {
                    budget.charge(1)?;
                    sibling = next;
                }
                hierarchy[sibling].next = Some(i);
                hierarchy[i].prev = Some(sibling);
            }
        }
    }
    Ok(hierarchy)
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f64 {
    let ac = p.x as f64 - a.x as f64;
    let bc = p.y as f64 - a.y as f64;
    let cc = b.x as f64 - a.x as f64;
    let dc = b.y as f64 - a.y as f64;
    let dot = ac * cc + bc * dc;
    let len_sq = cc * cc + dc * dc;
    let param = if len_sq != 0.0 { dot / len_sq } else { -1.0 };
    let (xx, yy) = if param < 0.0 {
        (a.x as f64, a.y as f64)
    } else if param > 1.0 {
        (b.x as f64, b.y as f64)
    } else {
        (a.x as f64 + param * cc, a.y as f64 + param * dc)
    };
    ((p.x as f64 - xx) * (p.x as f64 - xx) + (p.y as f64 - yy) * (p.y as f64 - yy)).sqrt()
}

fn rdp(points: &[Point], epsilon: f64, budget: &mut WorkBudget) -> Result<Vec<Point>, String> {
    if points.len() < 3 {
        return Ok(points.to_vec());
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((lo, hi)) = stack.pop() {
        let mut max_dist = 0.0;
        let mut max_index = 0usize;
        for index in lo + 1..hi {
            budget.charge(1)?;
            let d = distance_to_segment(points[index], points[lo], points[hi]);
            if d > max_dist {
                max_dist = d;
                max_index = index;
            }
        }
        if max_dist > epsilon {
            keep[max_index] = true;
            stack.push((lo, max_index));
            stack.push((max_index, hi));
        }
    }
    Ok(points
        .iter()
        .zip(&keep)
        .filter_map(|(point, &kept)| kept.then_some(*point))
        .collect())
}

fn approximate(
    contour: &[(i32, i32)],
    method: ApproximationMethod,
    budget: &mut WorkBudget,
) -> Result<Vec<Point>, String> {
    let points: Vec<Point> = contour
        .iter()
        .map(|&(x, y)| Point { x: x as u32, y: y as u32 })
        .collect();
    Ok(match method {
        ApproximationMethod::None => points,
        ApproximationMethod::Simple => {
            if points.len() < 3 {
                points
            } else {
                let mut result = Vec::with_capacity(points.len());
                result.push(points[0]);
                for index in 1..points.len() - 1 {
                    budget.charge(1)?;
                    let prev = contour[index - 1];
                    let curr = contour[index];
                    let next = contour[index + 1];
                    if (curr.0 - prev.0) * (next.1 - curr.1)
                        != (curr.1 - prev.1) * (next.0 - curr.0)
                    {
                        result.push(points[index]);
                    }
                }
                result.push(*points.last().unwrap());
                result
            }
        }
        ApproximationMethod::Tc89L1 | ApproximationMethod::Tc89Kcos => {
            rdp(&points, 2.0, budget)?
        }
    })
}

fn parse_mode(request: &Value) -> Result<RetrievalMode, String> {
    match request.get("mode") {
        None => Ok(RetrievalMode::External),
        Some(Value::String(name)) => match name.as_str() {
            "External" => Ok(RetrievalMode::External),
            "List" => Ok(RetrievalMode::List),
            "Ccomp" => Ok(RetrievalMode::Ccomp),
            "Tree" => Ok(RetrievalMode::Tree),
            other => Err(format!("unknown contour mode {other:?}")),
        },
        Some(_) => Err("mode must be a string".to_owned()),
    }
}

fn parse_approximation(request: &Value) -> Result<ApproximationMethod, String> {
    match request.get("approximation") {
        None => Ok(ApproximationMethod::Simple),
        Some(Value::String(name)) => match name.as_str() {
            "None" => Ok(ApproximationMethod::None),
            "Simple" => Ok(ApproximationMethod::Simple),
            "Tc89L1" => Ok(ApproximationMethod::Tc89L1),
            "Tc89Kcos" => Ok(ApproximationMethod::Tc89Kcos),
            other => Err(format!("unknown approximation {other:?}")),
        },
        Some(_) => Err("approximation must be a string".to_owned()),
    }
}

fn point_json(point: Point) -> Value {
    json!([point.x, point.y])
}

fn hierarchy_index(index: Option<usize>) -> i64 {
    index.map(|value| value as i64).unwrap_or(-1)
}

pub(crate) fn analyze(request: &Value) -> Result<Value, String> {
    let Some(method) = request["method"].as_str() else {
        return Err("cv.analyze requires a method string".to_owned());
    };
    if !matches!(method, "distance" | "thin" | "path" | "contours") {
        return Err(format!("unknown cv.analyze method {method:?}"));
    }
    let mode = parse_mode(request)?;
    let approximation = parse_approximation(request)?;
    let size = request["size"]
        .as_array()
        .filter(|size| size.len() == 2)
        .ok_or("cv.analyze requires size [width, height]")?;
    let width = size[0]
        .as_u64()
        .filter(|value| *value > 0 && *value <= u32::MAX as u64)
        .ok_or("mask width must be a positive integer")? as u32;
    let height = size[1]
        .as_u64()
        .filter(|value| *value > 0 && *value <= u32::MAX as u64)
        .ok_or("mask height must be a positive integer")? as u32;
    let total = (width as u64) * (height as u64);
    if total > QUERY_MAX_PIXELS {
        return Err(format!(
            "cv.analyze pixel count {total} exceeds query limit {QUERY_MAX_PIXELS}"
        ));
    }
    let raw = request["pixels"]
        .as_array()
        .ok_or("cv.analyze requires a pixels array")?;
    if raw.len() as u64 != total {
        return Err(format!(
            "pixels length {} does not match size {width}x{height}",
            raw.len()
        ));
    }
    let mut pixels = Vec::with_capacity(raw.len());
    for value in raw {
        let Some(pixel) = value.as_u64().filter(|pixel| *pixel <= u8::MAX as u64) else {
            return Err(format!("pixel {value} is not a u8 value"));
        };
        pixels.push(pixel as u8);
    }
    let mask = BinaryMask::new(width, height, pixels)?;
    let mut budget = WorkBudget::new(QUERY_BUDGET);
    match method {
        "distance" => {
            let mut result = json!({"method": "distance", "size": [width, height]});
            let object = result.as_object_mut().unwrap();
            match mask.distance_transform(&mut budget)? {
                DistanceTransform::NoBackground => {
                    object.insert("hasBackground".to_owned(), Value::Bool(false));
                    object.insert("distances".to_owned(), Value::Null);
                    object.insert("nearest".to_owned(), Value::Null);
                }
                DistanceTransform::Field(field) => {
                    object.insert("hasBackground".to_owned(), Value::Bool(true));
                    object.insert(
                        "distances".to_owned(),
                        json!(field
                            .samples()
                            .iter()
                            .map(|sample| f64::from(sample.distance))
                            .collect::<Vec<_>>()),
                    );
                    object.insert(
                        "nearest".to_owned(),
                        json!(field
                            .samples()
                            .iter()
                            .map(|sample| point_json(sample.nearest))
                            .collect::<Vec<_>>()),
                    );
                }
            }
            Ok(result)
        }
        "thin" => {
            let thinned = mask.thin(&mut budget)?;
            Ok(json!({
                "method": "thin",
                "size": [width, height],
                "pixels": thinned.pixels(),
            }))
        }
        "path" => {
            let path = mask.longest_path(&mut budget)?;
            Ok(json!({
                "method": "path",
                "size": [width, height],
                "path": path.iter().copied().map(point_json).collect::<Vec<_>>(),
            }))
        }
        "contours" => {
            let contours = mask.contours(mode, approximation, &mut budget)?;
            Ok(json!({
                "method": "contours",
                "size": [width, height],
                "contours": contours.points.iter()
                    .map(|contour| contour.iter().copied().map(point_json).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
                "hierarchy": contours.hierarchy.iter()
                    .map(|entry| [
                        hierarchy_index(entry.next),
                        hierarchy_index(entry.prev),
                        hierarchy_index(entry.child),
                        hierarchy_index(entry.parent),
                    ])
                    .collect::<Vec<_>>(),
            }))
        }
        _ => unreachable!(),
    }
}
