//! Animation system for bakawm.
//!
//! Supports easing curves and spring physics for smooth window animations.

mod easing;
mod spring;

pub use easing::{CubicBezier, Curve};
pub use spring::{Spring, SpringParams};

use std::time::{Duration, Instant};

/// A time-based animation from `from` to `to`.
#[derive(Debug, Clone)]
pub struct Animation {
    from: f64,
    to: f64,
    #[allow(dead_code)]
    initial_velocity: f64,
    is_off: bool,
    duration: Duration,
    /// Time until the animation first reaches `to`.
    ///
    /// Best effort; not always exactly precise.
    clamped_duration: Duration,
    start_time: Instant,
    kind: AnimationKind,
}

#[derive(Debug, Clone, Copy)]
enum AnimationKind {
    Easing { curve: Curve },
    Spring(Spring),
}

impl Animation {
    /// Create an animation that is effectively off (instantly reaches target).
    pub fn new_off() -> Self {
        return Self {
            from: 0.,
            to: 1.,
            initial_velocity: 0.,
            is_off: true,
            duration: Duration::ZERO,
            clamped_duration: Duration::ZERO,
            start_time: Instant::now(),
            kind: AnimationKind::Easing {
                curve: Curve::Linear,
            },
        }
    }

    /// Create an easing animation.
    pub fn ease(from: f64, to: f64, duration_ms: u32, curve: Curve) -> Self {
        let duration = Duration::from_millis(duration_ms as u64);
        let kind = AnimationKind::Easing { curve };

        return Self {
            from,
            to,
            initial_velocity: 0.,
            is_off: false,
            duration,
            // Our current curves never overshoot.
            clamped_duration: duration,
            start_time: Instant::now(),
            kind,
        }
    }

    /// Create a spring animation.
    pub fn spring(from: f64, to: f64, spring: Spring) -> Self {
        let duration = spring.duration();
        let clamped_duration = spring.clamped_duration().unwrap_or(duration);
        let kind = AnimationKind::Spring(Spring {
            from,
            to,
            initial_velocity: spring.initial_velocity,
            params: spring.params,
        });

        return Self {
            from,
            to,
            initial_velocity: spring.initial_velocity,
            is_off: false,
            duration,
            clamped_duration,
            start_time: Instant::now(),
            kind,
        }
    }

    /// Restart the animation with new from/to values, preserving the kind.
    pub fn restarted(&self, from: f64, to: f64, initial_velocity: f64) -> Self {
        if self.is_off {
            return self.clone();
        }

        match self.kind {
            AnimationKind::Easing { curve } => {
                return Self::ease(from, to, self.duration.as_millis() as u32, curve)
            }
            AnimationKind::Spring(spring) => {
                let spring = Spring {
                    from,
                    to,
                    initial_velocity,
                    params: spring.params,
                };
                return Self::spring(from, to, spring)
            }
        }
    }

    /// Whether the animation has completed.
    pub fn is_done(&self) -> bool {
        if self.is_off {
            return true;
        }

        return self.start_time.elapsed() >= self.duration
    }

    /// Whether the animation has reached its target value.
    pub fn is_clamped_done(&self) -> bool {
        if self.is_off {
            return true;
        }

        return self.start_time.elapsed() >= self.clamped_duration
    }

    /// Get the animation value at a specific elapsed duration.
    pub fn value_at(&self, elapsed: Duration) -> f64 {
        if elapsed >= self.duration {
            return self.to;
        }

        if self.is_off {
            return self.to;
        }

        match self.kind {
            AnimationKind::Easing { curve } => {
                let passed = elapsed.as_secs_f64();
                let total = self.duration.as_secs_f64();
                let x = (passed / total).clamp(0., 1.);
                return curve.y(x) * (self.to - self.from) + self.from
            }
            AnimationKind::Spring(spring) => {
                let value = spring.value_at(elapsed);

                // Protect against numerical instability.
                let range = (self.to - self.from) * 10.;
                let a = self.from - range;
                let b = self.to + range;
                if self.from <= self.to {
                    return value.clamp(a, b)
                } else {
                    return value.clamp(b, a)
                }
            }
        }
    }

    /// Get the current animation value.
    pub fn value(&self) -> f64 {
        if self.is_off {
            return self.to;
        }
        return self.value_at(self.start_time.elapsed())
    }

    /// Get a value that stops at the target value after first reaching it.
    ///
    /// Best effort; not always exactly precise.
    pub fn clamped_value(&self) -> f64 {
        if self.is_clamped_done() {
            return self.to;
        }

        return self.value()
    }

    pub fn from(&self) -> f64 {
        return self.from
    }

    pub fn to(&self) -> f64 {
        return self.to
    }

    pub fn duration(&self) -> Duration {
        return self.duration
    }
}
