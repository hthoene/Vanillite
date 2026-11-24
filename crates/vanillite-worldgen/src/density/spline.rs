//! `net.minecraft.util.CubicSpline` with density function coordinates (`SplineFunction`):
//! the parsed form, its `range()`, and the flat form a [`super::Program`] samples.

use std::sync::Arc;

use super::function::{DensityFunction, F32};
use crate::interval::Interval;
use crate::mth::{lerp_f, max_f, min_f};

/// A parsed cubic spline (`CubicSpline<SplineFunction.Coordinate>`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Spline {
    /// `CubicSpline.Constant`.
    Constant(F32),
    /// `CubicSpline.Multipoint`.
    Multipoint(Arc<Multipoint>),
}

/// `CubicSpline.Multipoint`: points in ascending location order.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct Multipoint {
    pub coordinate: DensityFunction,
    pub locations: Box<[F32]>,
    pub values: Box<[Spline]>,
    pub derivatives: Box<[F32]>,
}

/// `Multipoint.linearExtend(input, locations, value, derivatives, index)`.
#[inline]
fn linear_extend(input: f32, location: f32, value: f32, derivative: f32) -> f32 {
    if derivative == 0.0 {
        value
    } else {
        value + derivative * (input - location)
    }
}

impl Spline {
    /// `forEachCoordinate(consumer)`.
    pub fn for_each_coordinate(&self, consumer: &mut dyn FnMut(&DensityFunction)) {
        if let Spline::Multipoint(m) = self {
            consumer(&m.coordinate);
            for value in &m.values {
                value.for_each_coordinate(consumer);
            }
        }
    }

    /// `mapCoordinates(mapper)`: the coordinate first, then the values in order.
    pub fn map_coordinates(
        &self,
        mapper: &mut dyn FnMut(&DensityFunction) -> DensityFunction,
    ) -> Self {
        match self {
            Spline::Constant(_) => self.clone(),
            Spline::Multipoint(m) => {
                let coordinate = mapper(&m.coordinate);
                let values = m.values.iter().map(|v| v.map_coordinates(mapper)).collect();
                Spline::Multipoint(Arc::new(Multipoint {
                    coordinate,
                    locations: m.locations.clone(),
                    values,
                    derivatives: m.derivatives.clone(),
                }))
            }
        }
    }

    /// `range()`.
    pub fn range(&self) -> Interval {
        match self {
            Spline::Constant(value) => Interval::of_exact(value.0),
            Spline::Multipoint(m) => m.range(),
        }
    }
}

impl Multipoint {
    fn range(&self) -> Interval {
        let locations = &self.locations;
        let derivatives = &self.derivatives;
        let last = locations.len() - 1;
        let mut min_value = f32::INFINITY;
        let mut max_value = f32::NEG_INFINITY;
        let input = self.coordinate.range();
        if input.is_nai() {
            return input;
        }
        if input.min() < locations[0].0 {
            let first = self.values[0].range();
            let edge1 = linear_extend(input.min(), locations[0].0, first.min(), derivatives[0].0);
            let edge2 = linear_extend(input.min(), locations[0].0, first.max(), derivatives[0].0);
            min_value = min_f(min_value, min_f(edge1, edge2));
            max_value = max_f(max_value, max_f(edge1, edge2));
        }
        if input.max() > locations[last].0 {
            let range = self.values[last].range();
            let edge1 = linear_extend(
                input.max(),
                locations[last].0,
                range.min(),
                derivatives[last].0,
            );
            let edge2 = linear_extend(
                input.max(),
                locations[last].0,
                range.max(),
                derivatives[last].0,
            );
            min_value = min_f(min_value, min_f(edge1, edge2));
            max_value = max_f(max_value, max_f(edge1, edge2));
        }
        let ranges: Vec<Interval> = self.values.iter().map(Spline::range).collect();
        for range in &ranges {
            min_value = min_f(min_value, range.min());
            max_value = max_f(max_value, range.max());
        }
        for i in 0..last {
            let x1 = locations[i].0;
            let x2 = locations[i + 1].0;
            let x_diff = x2 - x1;
            let (min1, max1) = (ranges[i].min(), ranges[i].max());
            let (min2, max2) = (ranges[i + 1].min(), ranges[i + 1].max());
            let d1 = derivatives[i].0;
            let d2 = derivatives[i + 1].0;
            if d1 != 0.0 || d2 != 0.0 {
                let p1 = d1 * x_diff;
                let p2 = d2 * x_diff;
                let min_lerp1 = min_f(min1, min2);
                let max_lerp1 = max_f(max1, max2);
                let min_a = p1 - max2 + min1;
                let max_a = p1 - min2 + max1;
                let min_b = -p2 + min2 - max1;
                let max_b = -p2 + max2 - min1;
                let min_lerp2 = min_f(min_a, min_b);
                let max_lerp2 = max_f(max_a, max_b);
                min_value = min_f(min_value, min_lerp1 + 0.25 * min_lerp2);
                max_value = max_f(max_value, max_lerp1 + 0.25 * max_lerp2);
            }
        }
        Interval::of(min_value, max_value)
    }
}

/// A spline value in a [`SplineProgram`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SplineValue {
    /// `CubicSpline.Constant`.
    Constant(f32),
    /// Index into [`SplineProgram::multipoints`].
    Multipoint(u32),
}

/// A multipoint spline in a [`SplineProgram`]: its points are
/// `points[first_point .. first_point + point_count]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplineMultipoint {
    /// Index into [`SplineProgram::coordinates`].
    pub coordinate: u32,
    pub first_point: u32,
    pub point_count: u32,
}

/// One point of a multipoint spline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SplinePoint {
    pub location: f32,
    pub derivative: f32,
    pub value: SplineValue,
}

/// A compiled `SplineFunction.Sampler`: the spline as flat tables.
///
/// Equal coordinate functions share one coordinate slot (Vanilla's per-spline coordinate map),
/// so each is sampled at most once per point or volume.
#[derive(Clone, Debug, PartialEq)]
pub struct SplineProgram {
    /// The instruction of each coordinate slot.
    pub coordinates: Vec<u32>,
    pub root: SplineValue,
    pub multipoints: Vec<SplineMultipoint>,
    pub points: Vec<SplinePoint>,
}

impl SplineProgram {
    /// `CubicSpline.sample(spline, input)`; `coordinate(slot)` samples a coordinate.
    pub fn sample(&self, value: SplineValue, coordinate: &mut dyn FnMut(u32) -> f32) -> f32 {
        let m = match value {
            SplineValue::Constant(value) => return value,
            SplineValue::Multipoint(index) => self.multipoints[index as usize],
        };
        let points = &self.points[m.first_point as usize..(m.first_point + m.point_count) as usize];
        let input = coordinate(m.coordinate);
        // `findIntervalStart`: `Mth.binarySearch(0, n, i -> input < locations[i]) - 1`.
        let mut from = 0usize;
        let mut len = points.len();
        while len > 0 {
            let half = len / 2;
            let middle = from + half;
            if input < points[middle].location {
                len = half;
            } else {
                from = middle + 1;
                len -= half + 1;
            }
        }
        let last = points.len() - 1;
        if from == 0 {
            let p = points[0];
            linear_extend(
                input,
                p.location,
                self.sample(p.value, coordinate),
                p.derivative,
            )
        } else if from - 1 == last {
            let p = points[last];
            linear_extend(
                input,
                p.location,
                self.sample(p.value, coordinate),
                p.derivative,
            )
        } else {
            let p1 = points[from - 1];
            let p2 = points[from];
            let (x1, x2) = (p1.location, p2.location);
            let t = (input - x1) / (x2 - x1);
            let y1 = self.sample(p1.value, coordinate);
            let y2 = self.sample(p2.value, coordinate);
            let a = p1.derivative * (x2 - x1) - (y2 - y1);
            let b = -p2.derivative * (x2 - x1) + (y2 - y1);
            lerp_f(t, y1, y2) + t * (1.0 - t) * lerp_f(t, a, b)
        }
    }
}
