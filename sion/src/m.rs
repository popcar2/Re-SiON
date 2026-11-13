//! Portable float math so the crate needs only `core` (accurate to ~1e-6, plenty for audio).

use core::f32::consts::{FRAC_PI_2, LOG2_E, PI, TAU};

/// Round to nearest without float/int conversions (adding 1.5 * 2^23 drops the fraction).
fn round(x: f32) -> f32 { if x.abs() < 4194304.0 { (x + 12582912.0) - 12582912.0 } else { x } }

pub trait M {
    fn trunc(self) -> Self;
    fn floor(self) -> Self;
    fn fract(self) -> Self;
    fn rem_euclid(self, r: Self) -> Self;
    fn exp2(self) -> Self;
    fn log2(self) -> Self;
    fn powf(self, e: Self) -> Self;
    fn powi(self, n: i32) -> Self;
    fn sqrt(self) -> Self;
    fn sin(self) -> Self;
    fn cos(self) -> Self;
    fn sinh(self) -> Self;
}

impl M for f32 {
    fn trunc(self) -> f32 { if self < 0.0 { -M::floor(-self) } else { M::floor(self) } }
    fn floor(self) -> f32 { let r = round(self); if r > self { r - 1.0 } else { r } }
    fn fract(self) -> f32 { self - M::trunc(self) }
    fn rem_euclid(self, r: f32) -> f32 {
        let r = r.abs();
        let x = if r == 1.0 { self - M::floor(self) } else { self - r * M::floor(self / r) };
        if x >= r { 0.0 } else { x }
    }
    fn exp2(self) -> f32 {
        if self.is_nan() { return self }
        if self < -126.0 { return 0.0 }
        if self > 127.0 { return f32::INFINITY }
        let i = round(self);
        let f = self - i; // -0.5..0.5
        let p = 1.0 + f * (0.693_147_2 + f * (0.240_226_5 + f * (0.055_504_11 + f * (0.009_618_129 + f * (0.001_333_355_8 + f * 0.000_154_035_3)))));
        p * f32::from_bits(((i as i32 + 127) as u32) << 23)
    }
    fn log2(self) -> f32 {
        if !(self > 0.0) { return if self == 0.0 { f32::NEG_INFINITY } else { f32::NAN } }
        let b = self.to_bits();
        let m = f32::from_bits(b & 0x7f_ffff | 0x3f80_0000);
        let (s, e) = ((m - 1.0) / (m + 1.0), ((b >> 23) & 255) as i32 - 127);
        let s2 = s * s;
        e as f32 + 2.0 * LOG2_E * s * (1.0 + s2 * (1.0 / 3.0 + s2 * (0.2 + s2 * (1.0 / 7.0 + s2 * (1.0 / 9.0 + s2 * (1.0 / 11.0))))))
    }
    fn powf(self, e: f32) -> f32 { if self == 0.0 { 0.0 } else { M::exp2(e * M::log2(self)) } }
    fn powi(self, n: i32) -> f32 { let mut r = 1.0; for _ in 0..n.unsigned_abs() { r *= self } if n < 0 { 1.0 / r } else { r } }
    fn sqrt(self) -> f32 {
        if !(self > 0.0) { return if self == 0.0 { 0.0 } else { f32::NAN } }
        let mut y = f32::from_bits((self.to_bits() >> 1) + 0x1fbd_1df5);
        for _ in 0..3 { y = 0.5 * (y + self / y) }
        y
    }
    fn sin(self) -> f32 {
        let mut x = self - TAU * round(self * (1.0 / TAU));
        if x > FRAC_PI_2 { x = PI - x } else if x < -FRAC_PI_2 { x = -PI - x }
        let x2 = x * x;
        x * (1.0 - x2 * (1.0 / 6.0) * (1.0 - x2 * (1.0 / 20.0) * (1.0 - x2 * (1.0 / 42.0) * (1.0 - x2 * (1.0 / 72.0) * (1.0 - x2 * (1.0 / 110.0))))))
    }
    fn cos(self) -> f32 { M::sin(self + FRAC_PI_2) }
    fn sinh(self) -> f32 {
        if self.abs() < 0.5 { let x2 = self * self; return self * (1.0 + x2 * (1.0 / 6.0) * (1.0 + x2 * (1.0 / 20.0) * (1.0 + x2 * (1.0 / 42.0)))) }
        let e = M::exp2(self * LOG2_E);
        0.5 * (e - 1.0 / e)
    }
}
