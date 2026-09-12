/// Easing curves for animations.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Curve {
    Linear,
    EaseOutQuad,
    EaseOutCubic,
    EaseOutExpo,
    CubicBezier { x1: f64, y1: f64, x2: f64, y2: f64 },
}

impl Curve {
    /// Compute the y value for a given x in [0, 1].
    pub fn y(self, x: f64) -> f64 {
        match self {
            Curve::Linear => return x,
            Curve::EaseOutQuad => return ease_out_quad(x),
            Curve::EaseOutCubic => return ease_out_cubic(x),
            Curve::EaseOutExpo => return ease_out_expo(x),
            Curve::CubicBezier { x1, y1, x2, y2 } => return CubicBezier::new(x1, y1, x2, y2).y(x),
        }
    }
}

#[inline]
fn ease_out_quad(x: f64) -> f64 {
    return 1. - (1. - x) * (1. - x)
}

#[inline]
fn ease_out_cubic(x: f64) -> f64 {
    let t = 1. - x;
    return 1. - t * t * t
}

#[inline]
fn ease_out_expo(x: f64) -> f64 {
    if x <= 0. {
        return 0.
    } else {
        return 1. - 2f64.powf(-10. * x)
    }
}

/// Cubic Bézier easing curve.
///
/// Based on libadwaita (LGPL-2.1-or-later):
/// <https://gitlab.gnome.org/GNOME/libadwaita/-/blob/1.7.6/src/adw-easing.c?ref_type=tags#L469-531>
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

impl CubicBezier {
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        return Self { x1, y1, x2, y2 }
    }

    fn x_for_t(&self, t: f64) -> f64 {
        let omt = 1. - t;
        return 3. * omt * omt * t * self.x1 + 3. * omt * t * t * self.x2 + t * t * t
    }

    fn y_for_t(&self, t: f64) -> f64 {
        let omt = 1. - t;
        return 3. * omt * omt * t * self.y1 + 3. * omt * t * t * self.y2 + t * t * t
    }

    fn t_for_x(&self, x: f64) -> f64 {
        let mut min_t = 0.;
        let mut max_t = 1.;

        for _ in 0..=30 {
            let guess_t = (min_t + max_t) / 2.;
            let guess_x = self.x_for_t(guess_t);

            if x < guess_x {
                max_t = guess_t;
            } else {
                min_t = guess_t;
            }
        }

        return (min_t + max_t) / 2.
    }

    /// Compute the y value for a given x in [0, 1].
    pub fn y(&self, x: f64) -> f64 {
        if x <= f64::EPSILON {
            return 0.;
        }

        if 1. - f64::EPSILON <= x {
            return 1.;
        }

        return self.y_for_t(self.t_for_x(x))
    }
}
