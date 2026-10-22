//! Input stabilisation applied to raw pointer samples before dab placement.

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

use crate::brush::SmoothingMethod;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokePoint {
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
    /// Pen tilt away from vertical, 0 (upright) ..= 1 (flat); 0 for a mouse.
    #[serde(default)]
    pub tilt: f32,
    /// Which way the pen leans, degrees clockwise from the top of the screen
    /// (meaningful while `tilt` > 0).
    #[serde(default)]
    pub azimuth: f32,
    /// Seconds since some fixed instant (only differences matter); drives velocity.
    #[serde(default)]
    pub time: f32,
}

impl StrokePoint {
    pub fn new(x: f32, y: f32, pressure: f32) -> Self {
        Self { x, y, pressure, tilt: 0.0, azimuth: 0.0, time: 0.0 }
    }

    pub fn with_tilt(self, tilt: f32) -> Self {
        Self { tilt: tilt.clamp(0.0, 1.0), ..self }
    }

    pub fn with_azimuth(self, azimuth: f32) -> Self {
        Self { azimuth: azimuth.rem_euclid(360.0), ..self }
    }

    pub fn at_time(self, time: f32) -> Self {
        Self { time, ..self }
    }

    pub fn distance(self, other: StrokePoint) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }

    pub fn lerp(self, other: StrokePoint, t: f32) -> StrokePoint {
        let mix = |a: f32, b: f32| a + (b - a) * t;
        StrokePoint {
            x: mix(self.x, other.x),
            y: mix(self.y, other.y),
            pressure: mix(self.pressure, other.pressure),
            tilt: mix(self.tilt, other.tilt),
            // The short way round.
            azimuth: (self.azimuth + ((other.azimuth - self.azimuth + 540.0).rem_euclid(360.0) - 180.0) * t).rem_euclid(360.0),
            time: mix(self.time, other.time),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Smoother {
    Passthrough,
    Average { window: usize, samples: VecDeque<StrokePoint> },
    Pulled { length: f32, pos: Option<StrokePoint> },
}

impl Smoother {
    /// `stabilization` is 0..=100; 0 disables smoothing.
    pub fn new(method: SmoothingMethod, stabilization: u32) -> Self {
        let s = stabilization.min(100);
        if s == 0 {
            return Smoother::Passthrough;
        }
        match method {
            SmoothingMethod::Average => {
                Smoother::Average { window: 1 + (s as usize * 3) / 10, samples: VecDeque::new() }
            }
            SmoothingMethod::Pulled => Smoother::Pulled { length: s as f32 * 0.6, pos: None },
        }
    }

    /// Feeds a raw sample; returns the smoothed position to draw towards, if it moved.
    pub fn push(&mut self, p: StrokePoint) -> Option<StrokePoint> {
        match self {
            Smoother::Passthrough => Some(p),
            Smoother::Average { window, samples } => {
                samples.push_back(p);
                while samples.len() > *window {
                    samples.pop_front();
                }
                let n = samples.len() as f32;
                let sum = samples
                    .iter()
                    .fold([0.0f32; 4], |acc, s| [acc[0] + s.x, acc[1] + s.y, acc[2] + s.pressure, acc[3] + s.tilt]);
                // Time and lean direction are the newest sample's: smoothing lags position, not the clock.
                let (time, azimuth) = samples.back().map_or((0.0, 0.0), |s| (s.time, s.azimuth));
                Some(StrokePoint::new(sum[0] / n, sum[1] / n, sum[2] / n).with_tilt(sum[3] / n).with_azimuth(azimuth).at_time(time))
            }
            Smoother::Pulled { length, pos } => {
                let Some(cur) = pos else {
                    *pos = Some(p);
                    return Some(p);
                };
                let d = cur.distance(p);
                if d <= *length {
                    cur.pressure = p.pressure;
                    cur.tilt = p.tilt;
                    cur.azimuth = p.azimuth;
                    cur.time = p.time;
                    return None;
                }
                let moved = StrokePoint { pressure: p.pressure, tilt: p.tilt, azimuth: p.azimuth, time: p.time, ..cur.lerp(p, (d - *length) / d) };
                *cur = moved;
                Some(moved)
            }
        }
    }

    /// Points that let the stroke catch up with the last input when the pen lifts.
    pub fn finish(&mut self) -> Vec<StrokePoint> {
        match self {
            Smoother::Average { window, samples } => {
                let Some(&last) = samples.back() else {
                    return Vec::new();
                };
                let steps = window.saturating_sub(1);
                (0..steps).filter_map(|_| self.push(last)).collect()
            }
            // A pulled string deliberately lags; leaving it short is the expected feel.
            Smoother::Passthrough | Smoother::Pulled { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn average_converges_on_finish() {
        let mut s = Smoother::new(SmoothingMethod::Average, 30);
        s.push(StrokePoint::new(0.0, 0.0, 1.0));
        let mid = s.push(StrokePoint::new(10.0, 0.0, 1.0)).unwrap();
        assert!(mid.x < 10.0);
        let tail = s.finish();
        assert!((tail.last().unwrap().x - 10.0).abs() < 1e-4);
    }

    #[test]
    fn pulled_keeps_string_length() {
        let mut s = Smoother::new(SmoothingMethod::Pulled, 10); // length 6
        s.push(StrokePoint::new(0.0, 0.0, 1.0));
        assert!(s.push(StrokePoint::new(5.0, 0.0, 1.0)).is_none());
        let p = s.push(StrokePoint::new(20.0, 0.0, 1.0)).unwrap();
        assert!((p.x - 14.0).abs() < 1e-4);
    }
}
