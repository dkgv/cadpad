//! Quantities with units. A [`Quantity`] keeps the unit it was written in (for
//! printing) and normalises on demand to internal units.
use super::diagnostics::{Diagnostic, DiagnosticCode};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Physical dimension of a unit. Quantities only combine within a dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Dimension {
    Dimensionless,
    Length,
    Angle,
    Area,
    Volume,
    Mass,
    Time,
    Force,
    Impulse,
}

impl Dimension {
    /// Internal unit values of this dimension are normalised to.
    pub fn internal_unit(self) -> Unit {
        match self {
            Dimension::Dimensionless => Unit::One,
            Dimension::Length => Unit::Mm,
            Dimension::Angle => Unit::Rad,
            Dimension::Area => Unit::Mm2,
            Dimension::Volume => Unit::Mm3,
            Dimension::Mass => Unit::Kg,
            Dimension::Time => Unit::S,
            Dimension::Force => Unit::N,
            Dimension::Impulse => Unit::Ns,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Dimension::Dimensionless => "dimensionless",
            Dimension::Length => "length",
            Dimension::Angle => "angle",
            Dimension::Area => "area",
            Dimension::Volume => "volume",
            Dimension::Mass => "mass",
            Dimension::Time => "time",
            Dimension::Force => "force",
            Dimension::Impulse => "impulse",
        }
    }
}

/// Supported units. Internal units are mm, rad, mm2, mm3, kg, s, N and N s.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Unit {
    /// Dimensionless number (counts, unit-less parameters).
    #[serde(rename = "one")]
    One,
    #[serde(rename = "mm")]
    Mm,
    #[serde(rename = "cm")]
    Cm,
    #[serde(rename = "m")]
    M,
    /// Micrometre, µm.
    #[serde(rename = "um")]
    Um,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "deg")]
    Deg,
    #[serde(rename = "rad")]
    Rad,
    #[serde(rename = "mm2")]
    Mm2,
    #[serde(rename = "cm2")]
    Cm2,
    #[serde(rename = "mm3")]
    Mm3,
    #[serde(rename = "cm3")]
    Cm3,
    #[serde(rename = "g")]
    G,
    #[serde(rename = "kg")]
    Kg,
    #[serde(rename = "s")]
    S,
    #[serde(rename = "N")]
    N,
    /// Newton second.
    #[serde(rename = "N_s")]
    Ns,
}

impl Unit {
    pub const ALL: [Unit; 17] = [
        Unit::One,
        Unit::Mm,
        Unit::Cm,
        Unit::M,
        Unit::Um,
        Unit::In,
        Unit::Deg,
        Unit::Rad,
        Unit::Mm2,
        Unit::Cm2,
        Unit::Mm3,
        Unit::Cm3,
        Unit::G,
        Unit::Kg,
        Unit::S,
        Unit::N,
        Unit::Ns,
    ];
    pub fn dimension(self) -> Dimension {
        match self {
            Unit::One => Dimension::Dimensionless,
            Unit::Mm | Unit::Cm | Unit::M | Unit::Um | Unit::In => Dimension::Length,
            Unit::Deg | Unit::Rad => Dimension::Angle,
            Unit::Mm2 | Unit::Cm2 => Dimension::Area,
            Unit::Mm3 | Unit::Cm3 => Dimension::Volume,
            Unit::G | Unit::Kg => Dimension::Mass,
            Unit::S => Dimension::Time,
            Unit::N => Dimension::Force,
            Unit::Ns => Dimension::Impulse,
        }
    }
    /// Multiplier from this unit to the dimension's internal unit.
    pub fn factor(self) -> f64 {
        match self {
            Unit::One | Unit::Mm | Unit::Rad | Unit::Mm2 | Unit::Mm3 => 1.0,
            Unit::Kg | Unit::S | Unit::N | Unit::Ns => 1.0,
            Unit::Cm => 10.0,
            Unit::M => 1000.0,
            Unit::Um => 0.001,
            Unit::In => 25.4,
            Unit::Deg => std::f64::consts::PI / 180.0,
            Unit::Cm2 => 100.0,
            Unit::Cm3 => 1000.0,
            Unit::G => 0.001,
        }
    }
    /// Printed symbol.
    pub fn symbol(self) -> &'static str {
        match self {
            Unit::One => "",
            Unit::Mm => "mm",
            Unit::Cm => "cm",
            Unit::M => "m",
            Unit::Um => "µm",
            Unit::In => "in",
            Unit::Deg => "°",
            Unit::Rad => "rad",
            Unit::Mm2 => "mm²",
            Unit::Cm2 => "cm²",
            Unit::Mm3 => "mm³",
            Unit::Cm3 => "cm³",
            Unit::G => "g",
            Unit::Kg => "kg",
            Unit::S => "s",
            Unit::N => "N",
            Unit::Ns => "N·s",
        }
    }
}

impl fmt::Display for Unit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

/// A number with a unit. The unit is the one the author wrote; use
/// [`Quantity::normalized`] for comparisons and evaluation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub value: f64,
    pub unit: Unit,
}

impl Quantity {
    pub const fn new(value: f64, unit: Unit) -> Self {
        Self { value, unit }
    }
    pub fn dimension(&self) -> Dimension {
        self.unit.dimension()
    }
    /// Value in the dimension's internal unit (mm, rad, mm2, mm3, kg, s, N, N s).
    pub fn internal_value(&self) -> f64 {
        self.value * self.unit.factor()
    }
    /// This quantity expressed in its dimension's internal unit.
    pub fn normalized(&self) -> Quantity {
        Quantity::new(self.internal_value(), self.dimension().internal_unit())
    }
    /// Convert to another unit of the same dimension.
    pub fn convert_to(&self, unit: Unit) -> Result<Quantity, DimensionMismatch> {
        if unit.dimension() != self.dimension() {
            return Err(DimensionMismatch {
                expected: unit.dimension(),
                found: self.dimension(),
            });
        }
        Ok(Quantity::new(self.internal_value() / unit.factor(), unit))
    }
    /// Internal value, if the quantity has the expected dimension.
    pub fn expect(&self, dimension: Dimension) -> Result<f64, DimensionMismatch> {
        if self.dimension() == dimension {
            Ok(self.internal_value())
        } else {
            Err(DimensionMismatch {
                expected: dimension,
                found: self.dimension(),
            })
        }
    }
    /// Compare in internal units; `None` across dimensions or for NaN.
    pub fn partial_cmp_value(&self, other: &Quantity) -> Option<std::cmp::Ordering> {
        if self.dimension() != other.dimension() {
            return None;
        }
        self.internal_value().partial_cmp(&other.internal_value())
    }
    /// Equal after normalisation within a relative tolerance of 1e-9.
    pub fn same_value(&self, other: &Quantity) -> bool {
        if self.dimension() != other.dimension() {
            return false;
        }
        let (a, b) = (self.internal_value(), other.internal_value());
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-12)
    }
}

impl fmt::Display for Quantity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.unit {
            Unit::One => write!(f, "{}", self.value),
            Unit::Deg => write!(f, "{}°", self.value),
            unit => write!(f, "{} {}", self.value, unit),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DimensionMismatch {
    pub expected: Dimension,
    pub found: Dimension,
}

impl DimensionMismatch {
    /// `E_TYPE_UNIT` diagnostic at a JSON path.
    pub fn diagnostic(&self, at: impl Into<String>) -> Diagnostic {
        Diagnostic::new(DiagnosticCode::ETypeUnit, at, self.to_string())
    }
}

impl fmt::Display for DimensionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected a {} but found a {}",
            self.expected.name(),
            self.found.name()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() <= 1e-12 * b.abs().max(1.0), "{a} != {b}");
    }

    #[test]
    fn every_unit_normalises_to_its_internal_unit() {
        let cases = [
            (Unit::One, 3.0, 3.0, Unit::One),
            (Unit::Mm, 8.0, 8.0, Unit::Mm),
            (Unit::Cm, 2.5, 25.0, Unit::Mm),
            (Unit::M, 0.5, 500.0, Unit::Mm),
            (Unit::Um, 15.0, 0.015, Unit::Mm),
            (Unit::In, 1.0, 25.4, Unit::Mm),
            (Unit::Deg, 180.0, std::f64::consts::PI, Unit::Rad),
            (Unit::Deg, 110.0, 110f64.to_radians(), Unit::Rad),
            (Unit::Rad, 2.0, 2.0, Unit::Rad),
            (Unit::Mm2, 7.0, 7.0, Unit::Mm2),
            (Unit::Cm2, 2.0, 200.0, Unit::Mm2),
            (Unit::Mm3, 1e-5, 1e-5, Unit::Mm3),
            (Unit::Cm3, 2.0, 2000.0, Unit::Mm3),
            (Unit::G, 250.0, 0.25, Unit::Kg),
            (Unit::Kg, 3.0, 3.0, Unit::Kg),
            (Unit::S, 4.0, 4.0, Unit::S),
            (Unit::N, 9.0, 9.0, Unit::N),
            (Unit::Ns, 0.5, 0.5, Unit::Ns),
        ];
        let mut covered = std::collections::HashSet::new();
        for (unit, value, expected, internal) in cases {
            let normalized = Quantity::new(value, unit).normalized();
            approx(normalized.value, expected);
            assert_eq!(normalized.unit, internal, "{unit:?}");
            covered.insert(unit);
        }
        for unit in Unit::ALL {
            assert!(covered.contains(&unit), "{unit:?} has no conversion test");
        }
    }

    #[test]
    fn internal_units_have_unit_factor() {
        for unit in Unit::ALL {
            let internal = unit.dimension().internal_unit();
            assert_eq!(internal.dimension(), unit.dimension());
            assert_eq!(internal.factor(), 1.0);
        }
    }

    #[test]
    fn conversion_between_units_of_one_dimension() {
        let c = |v: f64, from: Unit, to: Unit| Quantity::new(v, from).convert_to(to).unwrap().value;
        approx(c(1.0, Unit::In, Unit::Mm), 25.4);
        approx(c(25.4, Unit::Mm, Unit::In), 1.0);
        approx(c(1.0, Unit::M, Unit::Um), 1e6);
        approx(c(1.0, Unit::Rad, Unit::Deg), 180.0 / std::f64::consts::PI);
        approx(c(1.0, Unit::Kg, Unit::G), 1000.0);
        approx(c(1.0, Unit::Cm3, Unit::Mm3), 1000.0);
        approx(c(1.0, Unit::Cm2, Unit::Mm2), 100.0);
    }

    #[test]
    fn dimension_mismatch_is_reported() {
        let error = Quantity::new(1.0, Unit::Mm)
            .convert_to(Unit::Deg)
            .unwrap_err();
        assert_eq!(error.expected, Dimension::Angle);
        assert_eq!(error.found, Dimension::Length);
        assert_eq!(
            error.diagnostic("claims[0]").code,
            DiagnosticCode::ETypeUnit
        );
        assert!(
            Quantity::new(1.0, Unit::Mm)
                .expect(Dimension::Length)
                .is_ok()
        );
        assert!(
            Quantity::new(1.0, Unit::Mm)
                .expect(Dimension::Area)
                .is_err()
        );
        assert!(
            Quantity::new(1.0, Unit::Mm)
                .partial_cmp_value(&Quantity::new(1.0, Unit::S))
                .is_none()
        );
    }

    #[test]
    fn comparison_and_equality_use_internal_units() {
        use std::cmp::Ordering::*;
        assert!(Quantity::new(25.4, Unit::Mm).same_value(&Quantity::new(1.0, Unit::In)));
        assert_eq!(
            Quantity::new(1.0, Unit::Cm).partial_cmp_value(&Quantity::new(9.0, Unit::Mm)),
            Some(Greater)
        );
        assert_eq!(
            Quantity::new(15.0, Unit::Um).partial_cmp_value(&Quantity::new(0.015, Unit::Mm)),
            Some(Equal)
        );
        assert!(
            Quantity::new(90.0, Unit::Deg)
                .same_value(&Quantity::new(std::f64::consts::FRAC_PI_2, Unit::Rad))
        );
    }

    #[test]
    fn original_unit_is_kept_for_printing() {
        assert_eq!(Quantity::new(110.0, Unit::Deg).to_string(), "110°");
        assert_eq!(Quantity::new(8.0, Unit::Mm).to_string(), "8 mm");
        assert_eq!(Quantity::new(1e-5, Unit::Mm3).to_string(), "0.00001 mm³");
        assert_eq!(Quantity::new(0.25, Unit::Ns).to_string(), "0.25 N·s");
        assert_eq!(Quantity::new(15.0, Unit::Um).to_string(), "15 µm");
        assert_eq!(Quantity::new(5.0, Unit::One).to_string(), "5");
    }

    #[test]
    fn serde_uses_plain_unit_names() {
        let q: Quantity = serde_json::from_str(r#"{"value":2,"unit":"N_s"}"#).unwrap();
        assert_eq!(q.unit, Unit::Ns);
        assert_eq!(
            serde_json::to_string(&Quantity::new(1.0, Unit::Um)).unwrap(),
            r#"{"value":1.0,"unit":"um"}"#
        );
        assert!(serde_json::from_str::<Quantity>(r#"{"value":2,"unit":"furlong"}"#).is_err());
        assert!(serde_json::from_str::<Quantity>(r#"{"value":2,"unit":"mm","x":1}"#).is_err());
    }
}
