//! NTC thermistor conversion for a voltage divider.
//!
//! The thermistor is the upper leg: VCC, the thermistor, the ADC
//! input, the fixed resistor, ground. An open thermistor reads
//! 0 V and a shorted one reads VCC, and the voltage rises with
//! temperature.

use serde::Deserialize;

/// Divider and Steinhart-Hart parameters for one thermistor.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Thermistor {
    /// The fixed resistor in the divider, in ohms.
    pub divider_resistor: f64,
    /// Steinhart-Hart coefficients.
    pub a: f64,
    pub b: f64,
    pub c: f64,
}

impl Thermistor {
    /// Converts the divider's output voltage to degrees Celsius.
    pub fn celsius(&self, volts: f64, vcc: f64) -> f64 {
        self.resistance_to_celsius(self.resistance(volts, vcc))
    }

    /// Solves the divider for the thermistor's resistance.
    pub fn resistance(&self, volts: f64, vcc: f64) -> f64 {
        self.divider_resistor * (vcc - volts) / volts
    }

    /// Steinhart-Hart: 1/T = a + b ln R + c (ln R)^3.
    pub fn resistance_to_celsius(&self, ohms: f64) -> f64 {
        let ln_r = ohms.ln();
        1.0 / (self.a + self.b * ln_r + self.c * ln_r.powi(3)) - 273.15
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 10 k NTC with B = 3950, from the B-parameter model:
    /// a = 1/T0 - ln(R0)/B, b = 1/B, c = 0.
    fn generic_10k() -> Thermistor {
        Thermistor {
            divider_resistor: 10_000.0,
            a: 1.0 / 298.15 - 10_000f64.ln() / 3950.0,
            b: 1.0 / 3950.0,
            c: 0.0,
        }
    }

    #[test]
    fn reads_25c_at_nominal_resistance() {
        let t = generic_10k().resistance_to_celsius(10_000.0);
        assert!((t - 25.0).abs() < 0.01, "{t}");
    }

    #[test]
    fn ntc_reads_hotter_at_lower_resistance() {
        let th = generic_10k();
        assert!(th.resistance_to_celsius(4_000.0) > th.resistance_to_celsius(20_000.0));
    }

    #[test]
    fn divider_midpoint_is_the_fixed_resistor() {
        let r = generic_10k().resistance(1.65, 3.3);
        assert!((r - 10_000.0).abs() < 0.1, "{r}");
    }

    #[test]
    fn voltage_rises_with_temperature() {
        let th = generic_10k();
        assert!(th.celsius(2.0, 3.3) > th.celsius(1.0, 3.3));
    }

    #[test]
    fn midpoint_voltage_reads_25c() {
        let t = generic_10k().celsius(1.65, 3.3);
        assert!((t - 25.0).abs() < 0.01, "{t}");
    }
}
