//! The 2×3 affine transform PDF uses for both text and graphics state.

#[derive(Debug, Clone, Copy)]
pub(super) struct Matrix {
    pub(super) a: f32,
    pub(super) b: f32,
    pub(super) c: f32,
    pub(super) d: f32,
    pub(super) e: f32,
    pub(super) f: f32,
}

impl Matrix {
    pub(super) fn identity() -> Self {
        Self {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// The six operands `Tm` and `cm` both take, in order.
    pub(super) fn from_nums(nums: &[f32]) -> Option<Self> {
        let [a, b, c, d, e, f, ..] = nums else {
            return None;
        };
        Some(Self {
            a: *a,
            b: *b,
            c: *c,
            d: *d,
            e: *e,
            f: *f,
        })
    }

    pub(super) fn is_identity(&self) -> bool {
        self.a == 1.0
            && self.b == 0.0
            && self.c == 0.0
            && self.d == 1.0
            && self.e == 0.0
            && self.f == 0.0
    }

    /// Map a point through the transform.
    pub(super) fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        if self.is_identity() {
            return (x, y);
        }
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Pre-multiply: `self = other * self` (translate-by-other semantics
    /// matches how PDF accumulates `Td` and `Tm` against the line matrix).
    pub(super) fn translate(&mut self, tx: f32, ty: f32) {
        self.e += tx * self.a + ty * self.c;
        self.f += tx * self.b + ty * self.d;
    }

    /// `self × m`, the order `cm` concatenates onto the current CTM.
    pub(super) fn concat(&self, m: Matrix) -> Matrix {
        Matrix {
            a: self.a * m.a + self.b * m.c,
            b: self.a * m.b + self.b * m.d,
            c: self.c * m.a + self.d * m.c,
            d: self.c * m.b + self.d * m.d,
            e: self.e * m.a + self.f * m.c + m.e,
            f: self.e * m.b + self.f * m.d + m.f,
        }
    }

    /// Horizontal and vertical scale of the transformed basis vectors.
    pub(super) fn basis_lengths(&self) -> (f32, f32) {
        (self.a.hypot(self.b), self.c.hypot(self.d))
    }
}

/// Has the writing direction turned enough that the two runs are separate
/// words? Rotated labels and vertical stamps show up this way.
pub(super) fn text_direction_changed(previous: Matrix, next: Matrix) -> bool {
    let (previous_length, _) = previous.basis_lengths();
    let (next_length, _) = next.basis_lengths();
    if previous_length <= f32::EPSILON || next_length <= f32::EPSILON {
        return false;
    }
    let dot = previous.a * next.a + previous.b * next.b;
    dot < previous_length * next_length * 0.99
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_maps_points_unchanged_and_reports_itself() {
        let m = Matrix::identity();
        assert!(m.is_identity());
        assert_eq!(m.apply(3.0, 4.0), (3.0, 4.0));
        assert_eq!(m.basis_lengths(), (1.0, 1.0));
    }

    #[test]
    fn from_nums_needs_six_operands() {
        assert!(Matrix::from_nums(&[1.0, 0.0, 0.0, 1.0, 5.0]).is_none());
        let m = Matrix::from_nums(&[2.0, 0.0, 0.0, 3.0, 5.0, 7.0, 9.0]).unwrap();
        assert!(!m.is_identity());
        assert_eq!(m.apply(1.0, 1.0), (7.0, 10.0));
    }

    #[test]
    fn translate_and_concat_accumulate() {
        let mut m = Matrix::from_nums(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]).unwrap();
        m.translate(3.0, 4.0);
        assert_eq!((m.e, m.f), (6.0, 8.0));

        let scale = Matrix::from_nums(&[2.0, 0.0, 0.0, 2.0, 0.0, 0.0]).unwrap();
        let shift = Matrix::from_nums(&[1.0, 0.0, 0.0, 1.0, 10.0, 20.0]).unwrap();
        assert_eq!(scale.concat(shift).apply(1.0, 1.0), (12.0, 22.0));
    }

    #[test]
    fn direction_change_needs_two_non_degenerate_bases() {
        let horizontal = Matrix::identity();
        let rotated = Matrix::from_nums(&[0.0, 1.0, -1.0, 0.0, 0.0, 0.0]).unwrap();
        let degenerate = Matrix::from_nums(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0]).unwrap();
        assert!(text_direction_changed(horizontal, rotated));
        assert!(!text_direction_changed(horizontal, horizontal));
        assert!(!text_direction_changed(degenerate, rotated));
        assert!(!text_direction_changed(horizontal, degenerate));
    }
}
