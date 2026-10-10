// Copyright 2026 The servoaf Authors
// SPDX-License-Identifier: MPL-2.0

//! Human-like mouse movement: a curved cubic Bézier path with a minimum-jerk
//! velocity profile (slow-fast-slow), small tremor that fades near the target,
//! irregular sampling and an occasional overshoot-and-correct.

use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    fn add(self, dx: f64, dy: f64) -> Self {
        Self::new(self.x + dx, self.y + dy)
    }
    pub fn distance(self, other: Point) -> f64 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PathPoint {
    pub position: Point,
    /// Offset from the start of the movement.
    pub time: Duration,
}

#[derive(Clone, Debug)]
pub struct PathOptions {
    /// Total duration; derived from Fitts's law using `target_width` if unset.
    pub duration: Option<Duration>,
    /// Approximate size of the thing being pointed at, in CSS px.
    pub target_width: f64,
    /// Interval between emitted moves (random per step, like a real mouse's
    /// irregular report rate).
    pub min_step: Duration,
    pub max_step: Duration,
    /// Maximum hand-tremor jitter in CSS px, fading to zero at the target.
    pub jitter: f64,
    /// Whether long movements may overshoot slightly and correct.
    pub allow_overshoot: bool,
}

impl Default for PathOptions {
    fn default() -> Self {
        Self {
            duration: None,
            target_width: 24.0,
            min_step: Duration::from_millis(7),
            max_step: Duration::from_millis(16),
            jitter: 0.8,
            allow_overshoot: true,
        }
    }
}

const MIN_MOVE_MS: f64 = 90.0;
const MAX_MOVE_MS: f64 = 1600.0;

/// A source of uniform random numbers in [0, 1).
pub trait Random {
    fn uniform(&mut self) -> f64;
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }
    /// Roughly normal in [-1, 1] (sum of uniforms).
    fn centered(&mut self) -> f64 {
        (self.uniform() + self.uniform() + self.uniform()) / 1.5 - 1.0
    }
}

pub struct ThreadRandom;

impl Random for ThreadRandom {
    fn uniform(&mut self) -> f64 {
        rand::random::<f64>()
    }
}

/// Movement time predicted by Fitts's law (Shannon formulation), clamped to a
/// plausible human range.
pub fn fitts_duration(distance: f64, target_width: f64) -> Duration {
    // T = a + b * log2(D / W + 1), with constants typical for mouse pointing.
    const A: f64 = 110.0;
    const B: f64 = 140.0;
    let width = target_width.max(1.0);
    let ms = A + B * (distance.max(0.0) / width + 1.0).log2();
    Duration::from_secs_f64(ms.clamp(MIN_MOVE_MS, MAX_MOVE_MS) / 1000.0)
}

fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (b0, b1, b2, b3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(b0 * p0.x + b1 * p1.x + b2 * p2.x + b3 * p3.x, b0 * p0.y + b1 * p1.y + b2 * p2.y + b3 * p3.y)
}

/// Minimum-jerk position profile: smooth acceleration and deceleration.
fn minimum_jerk(t: f64) -> f64 {
    t * t * t * (10.0 + t * (-15.0 + 6.0 * t))
}

/// Appends one curved segment from `from` to `to`. The first point (where the
/// cursor already is) is not emitted.
fn append_segment(
    from: Point,
    to: Point,
    start: Duration,
    duration: Duration,
    options: &PathOptions,
    rng: &mut dyn Random,
    out: &mut Vec<PathPoint>,
) {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let distance = dx.hypot(dy);
    let (nx, ny) = if distance > 0.0 { (-dy / distance, dx / distance) } else { (0.0, 0.0) };

    // Curvature grows with distance but is capped; people arc more on long
    // movements. Both control points bend to the same side most of the time.
    let max_bend = (distance * 0.22).min(160.0);
    let side = if rng.uniform() < 0.5 { -1.0 } else { 1.0 };
    let bend1 = side * rng.range(0.2, 1.0) * max_bend;
    let bend2 = side * rng.range(-0.2, 0.8) * max_bend;
    let t1 = rng.range(0.2, 0.4);
    let t2 = rng.range(0.6, 0.85);
    let c1 = from.add(dx * t1 + nx * bend1, dy * t1 + ny * bend1);
    let c2 = from.add(dx * t2 + nx * bend2, dy * t2 + ny * bend2);

    let min_step = options.min_step.as_secs_f64() * 1000.0;
    let max_step = (options.max_step.as_secs_f64() * 1000.0).max(min_step);
    let total = (duration.as_secs_f64() * 1000.0).max(min_step);

    let mut elapsed = 0.0;
    loop {
        elapsed += rng.range(min_step, max_step).max(1.0);
        let last = elapsed >= total;
        if last {
            elapsed = total;
        }
        let s = minimum_jerk(elapsed / total);
        let mut p = if last { to } else { bezier(from, c1, c2, to, s) };
        if !last && options.jitter > 0.0 {
            // Tremor fades out as the hand homes in on the target.
            let amplitude = options.jitter * (1.0 - s);
            p = p.add(rng.centered() * amplitude, rng.centered() * amplitude);
        }
        out.push(PathPoint { position: p, time: start + Duration::from_secs_f64(elapsed / 1000.0) });
        if last {
            return;
        }
    }
}

/// The cursor positions a person would produce moving from `from` to `to`. The
/// last point is exactly `to`; times are strictly increasing.
pub fn human_mouse_path(
    from: Point,
    to: Point,
    options: &PathOptions,
    rng: &mut dyn Random,
) -> Vec<PathPoint> {
    let distance = from.distance(to);
    if distance < 0.5 {
        return vec![PathPoint { position: to, time: Duration::ZERO }];
    }
    let duration = options
        .duration
        .unwrap_or_else(|| fitts_duration(distance, options.target_width).mul_f64(rng.range(0.85, 1.15)))
        .max(options.min_step);

    let mut path = Vec::new();
    let overshoot = options.allow_overshoot && distance > 180.0 && rng.uniform() < 0.3;
    if !overshoot {
        append_segment(from, to, Duration::ZERO, duration, options, rng, &mut path);
        return path;
    }

    // Overshoot past the target along the direction of travel, then make a
    // short corrective movement back.
    let (ux, uy) = ((to.x - from.x) / distance, (to.y - from.y) / distance);
    let (nx, ny) = (-uy, ux);
    let along = rng.range(4.0, 14.0);
    let across = rng.range(-5.0, 5.0);
    let overshoot_point = to.add(ux * along + nx * across, uy * along + ny * across);
    append_segment(from, overshoot_point, Duration::ZERO, duration.mul_f64(0.85), options, rng, &mut path);
    let correction = Duration::from_secs_f64(rng.range(70.0, 160.0) / 1000.0);
    let correction_options = PathOptions { jitter: options.jitter * 0.5, ..options.clone() };
    let start = path.last().map_or(Duration::ZERO, |point| point.time);
    append_segment(overshoot_point, to, start, correction, &correction_options, rng, &mut path);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic xorshift for reproducible tests.
    struct TestRandom(u64);
    impl Random for TestRandom {
        fn uniform(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[test]
    fn fitts_is_clamped_and_monotonic() {
        assert_eq!(fitts_duration(0.0, 24.0), Duration::from_millis(110));
        assert!(fitts_duration(100.0, 24.0) < fitts_duration(800.0, 24.0));
        assert!(fitts_duration(800.0, 4.0) > fitts_duration(800.0, 200.0));
        assert_eq!(fitts_duration(1e9, 1.0), Duration::from_millis(1600));
        assert_eq!(fitts_duration(0.0, 1e9), Duration::from_millis(110));
    }

    #[test]
    fn path_properties() {
        for seed in 1..200u64 {
            let mut rng = TestRandom(seed * 7919);
            let from = Point::new(10.0, 20.0);
            let to = Point::new(700.0, 450.0);
            let path = human_mouse_path(from, to, &PathOptions::default(), &mut rng);
            assert_eq!(path.last().unwrap().position, to);
            assert!(path.len() > 10);
            for pair in path.windows(2) {
                assert!(pair[1].time > pair[0].time);
                let gap = pair[1].time - pair[0].time;
                assert!(gap <= Duration::from_millis(17), "{gap:?}");
                // No teleporting.
                assert!(pair[0].position.distance(pair[1].position) < 200.0);
            }
            let total = path.last().unwrap().time;
            assert!(total >= Duration::from_millis(80) && total < Duration::from_millis(2200), "{total:?}");
            // Path stays in a sane envelope around the straight line.
            for point in &path {
                assert!(point.position.x > -200.0 && point.position.x < 1000.0);
                assert!(point.position.y > -200.0 && point.position.y < 800.0);
            }
        }
    }

    #[test]
    fn path_is_curved_and_slow_at_the_ends() {
        let mut rng = TestRandom(42);
        let options = PathOptions { allow_overshoot: false, jitter: 0.0, ..Default::default() };
        let path = human_mouse_path(Point::new(0.0, 0.0), Point::new(600.0, 0.0), &options, &mut rng);
        let max_deviation = path.iter().map(|p| p.position.y.abs()).fold(0.0, f64::max);
        assert!(max_deviation > 5.0, "path should arc, got {max_deviation}");
        let speed = |i: usize| {
            let (a, b) = (path[i], path[i + 1]);
            a.position.distance(b.position) / (b.time - a.time).as_secs_f64()
        };
        let mid = path.len() / 2;
        assert!(speed(0) < speed(mid));
        assert!(speed(path.len() - 2) < speed(mid));
    }

    #[test]
    fn tiny_moves_jump() {
        let mut rng = TestRandom(3);
        let path =
            human_mouse_path(Point::new(5.0, 5.0), Point::new(5.2, 5.1), &PathOptions::default(), &mut rng);
        assert_eq!(path.len(), 1);
    }

    #[test]
    fn explicit_duration() {
        let mut rng = TestRandom(9);
        let options = PathOptions {
            duration: Some(Duration::from_millis(500)),
            allow_overshoot: false,
            ..Default::default()
        };
        let path = human_mouse_path(Point::new(0.0, 0.0), Point::new(300.0, 300.0), &options, &mut rng);
        assert_eq!(path.last().unwrap().time, Duration::from_millis(500));
    }
}
