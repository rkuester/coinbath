//! Calibration points and the Steinhart-Hart fit.
//!
//! A `Point` is one reference temperature with the averaged
//! divider voltage of each probe that was at that temperature.
//! `fit` turns a probe's points into coefficients for the config.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::thermistor::Thermistor;

/// The file of points, `[[point]]` in TOML.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Points {
    #[serde(default, rename = "point")]
    pub points: Vec<Point>,
}

/// One reference temperature and the probes measured at it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// A name for the point, such as "ice" or "bath-warm".
    pub label: String,
    /// The reference thermometer's reading, in degrees Celsius.
    pub reference_c: f64,
    /// When the point was taken, as Unix seconds.
    pub taken: u64,
    pub samples: Vec<Sample>,
}

/// One probe's averaged measurement at a point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub probe: String,
    /// Mean divider voltage over the sampling window.
    pub volts: f64,
    /// Mean supply voltage over the same window.
    pub supply_v: f64,
    /// Largest minus smallest divider voltage in the window, a
    /// measure of how settled the probe was.
    pub spread_v: f64,
}

/// A fitted probe: coefficients and how far each point misses.
#[derive(Debug, Clone, PartialEq)]
pub struct Fit {
    pub thermistor: Thermistor,
    /// Each point's label, reference, and fitted minus reference.
    pub residuals: Vec<(String, f64, f64)>,
}

/// How many Steinhart-Hart coefficients a fit solves for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terms {
    /// `a` and `b`, with `c` = 0: the B-parameter model.
    Two,
    /// `a`, `b`, and `c`.
    Three,
}

impl Terms {
    /// Three coefficients need four or more points to be
    /// determined by more than the thermometer's rounding; fewer
    /// points get the B model.
    pub fn for_points(n: usize) -> Self {
        if n >= 4 { Terms::Three } else { Terms::Two }
    }
}

/// Fits `probe` from every point that measured it, with the
/// divider's fixed resistor `divider_resistor` in ohms, by least
/// squares over the chosen number of coefficients.
pub fn fit(points: &Points, probe: &str, divider_resistor: f64, terms: Terms) -> Result<Fit> {
    let mut rows: Vec<(String, f64, f64)> = Vec::new();
    for point in &points.points {
        if let Some(sample) = point.samples.iter().find(|s| s.probe == probe) {
            let ohms = divider_resistor * (sample.supply_v - sample.volts) / sample.volts;
            rows.push((point.label.clone(), point.reference_c, ohms));
        }
    }
    let thermistor = match (rows.len(), terms) {
        (0 | 1, _) => bail!(
            "probe {probe} has {} points; two or more are needed",
            rows.len()
        ),
        (2, Terms::Three) => bail!("probe {probe} has 2 points; three coefficients need 3"),
        (_, Terms::Two) => least_squares_two(divider_resistor, &rows)?,
        (_, Terms::Three) => least_squares(divider_resistor, &rows)?,
    };
    let residuals = rows
        .iter()
        .map(|(label, reference_c, ohms)| {
            let fitted = thermistor.resistance_to_celsius(*ohms);
            (label.clone(), *reference_c, fitted - reference_c)
        })
        .collect();
    Ok(Fit {
        thermistor,
        residuals,
    })
}

fn kelvin(celsius: f64) -> f64 {
    celsius + 273.15
}

/// Least squares for y = a + b x with x = ln R and y = 1/T.
fn least_squares_two(divider_resistor: f64, rows: &[(String, f64, f64)]) -> Result<Thermistor> {
    let n = rows.len() as f64;
    let (mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0);
    for (_, celsius, ohms) in rows {
        let x = ohms.ln();
        let y = 1.0 / kelvin(*celsius);
        sx += x;
        sy += y;
        sxx += x * x;
        sxy += x * y;
    }
    let det = n * sxx - sx * sx;
    if det.abs() < 1e-300 {
        bail!("points do not determine the coefficients");
    }
    let b = (n * sxy - sx * sy) / det;
    let a = (sy - b * sx) / n;
    Ok(Thermistor {
        divider_resistor,
        a,
        b,
        c: 0.0,
    })
}

fn least_squares(divider_resistor: f64, rows: &[(String, f64, f64)]) -> Result<Thermistor> {
    // Normal equations for y = a + b x + c x^3 with x = ln R and
    // y = 1/T.
    let mut m = [[0.0f64; 3]; 3];
    let mut v = [0.0f64; 3];
    for (_, celsius, ohms) in rows {
        let x = ohms.ln();
        let y = 1.0 / kelvin(*celsius);
        let basis = [1.0, x, x.powi(3)];
        for i in 0..3 {
            v[i] += basis[i] * y;
            for j in 0..3 {
                m[i][j] += basis[i] * basis[j];
            }
        }
    }
    let [a, b, c] = solve3(m, v)?;
    Ok(Thermistor {
        divider_resistor,
        a,
        b,
        c,
    })
}

/// Solves a 3x3 system by Gaussian elimination with pivoting.
fn solve3(mut m: [[f64; 3]; 3], mut v: [f64; 3]) -> Result<[f64; 3]> {
    for col in 0..3 {
        let pivot = (col..3)
            .max_by(|&i, &j| m[i][col].abs().total_cmp(&m[j][col].abs()))
            .expect("three rows");
        if m[pivot][col].abs() < 1e-300 {
            bail!("points do not determine the coefficients");
        }
        m.swap(col, pivot);
        v.swap(col, pivot);
        for row in 0..3 {
            if row != col {
                let factor = m[row][col] / m[col][col];
                let pivot_row = m[col];
                for (k, value) in m[row].iter_mut().enumerate().skip(col) {
                    *value -= factor * pivot_row[k];
                }
                v[row] -= factor * v[col];
            }
        }
    }
    Ok([v[0] / m[0][0], v[1] / m[1][1], v[2] / m[2][2]])
}

#[cfg(test)]
mod tests {
    use super::*;

    const R_FIXED: f64 = 10_000.0;

    /// A known thermistor to synthesize points from.
    fn truth() -> Thermistor {
        Thermistor {
            divider_resistor: R_FIXED,
            a: 1.125e-3,
            b: 2.347e-4,
            c: 8.566e-8,
        }
    }

    /// The divider voltage the thermistor gives at `celsius`,
    /// found by bisection on resistance.
    fn volts_at(th: &Thermistor, celsius: f64, supply: f64) -> f64 {
        let (mut lo, mut hi): (f64, f64) = (100.0, 1_000_000.0);
        for _ in 0..200 {
            let mid = (lo * hi).sqrt();
            if th.resistance_to_celsius(mid) > celsius {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let ohms = (lo * hi).sqrt();
        supply * R_FIXED / (ohms + R_FIXED)
    }

    fn points(th: &Thermistor, temps: &[f64]) -> Points {
        Points {
            points: temps
                .iter()
                .enumerate()
                .map(|(i, &c)| Point {
                    label: format!("p{i}"),
                    reference_c: c,
                    taken: 0,
                    samples: vec![Sample {
                        probe: "bath".into(),
                        volts: volts_at(th, c, 3.28),
                        supply_v: 3.28,
                        spread_v: 0.0,
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn three_points_recover_the_coefficients() {
        let fit = fit(
            &points(&truth(), &[0.0, 40.0, 80.0]),
            "bath",
            R_FIXED,
            Terms::Three,
        )
        .unwrap();
        let t = fit.thermistor;
        assert!((t.a - 1.125e-3).abs() < 1e-8, "a = {}", t.a);
        assert!((t.b - 2.347e-4).abs() < 1e-8, "b = {}", t.b);
        assert!((t.c - 8.566e-8).abs() < 1e-10, "c = {}", t.c);
        assert!(fit.residuals.iter().all(|(_, _, r)| r.abs() < 1e-6));
    }

    #[test]
    fn two_points_hit_both_and_stay_close_between() {
        let fit = fit(&points(&truth(), &[0.0, 50.0]), "bath", R_FIXED, Terms::Two).unwrap();
        assert!(fit.residuals.iter().all(|(_, _, r)| r.abs() < 1e-6));
        assert_eq!(fit.thermistor.c, 0.0);
        // The B model misses a real thermistor between the points,
        // but by well under a degree over this span.
        let v = volts_at(&truth(), 25.0, 3.28);
        let at_25 = fit.thermistor.celsius(v, 3.28);
        assert!((at_25 - 25.0).abs() < 0.5, "{at_25}");
    }

    #[test]
    fn one_point_is_refused() {
        assert!(fit(&points(&truth(), &[20.0]), "bath", R_FIXED, Terms::Two).is_err());
        assert!(
            fit(
                &points(&truth(), &[0.0, 50.0]),
                "inlet",
                R_FIXED,
                Terms::Two
            )
            .is_err()
        );
        assert!(
            fit(
                &points(&truth(), &[0.0, 50.0]),
                "bath",
                R_FIXED,
                Terms::Three
            )
            .is_err()
        );
    }

    #[test]
    fn two_terms_average_rounded_points() {
        // Three points over a narrow span, each reference rounded
        // to a whole degree Fahrenheit as a kitchen thermometer
        // reads. Two terms spread the rounding across the points
        // and stay physical; the residuals are bounded by it.
        let round_f = |c: f64| ((c * 9.0 / 5.0 + 32.0).round() - 32.0) * 5.0 / 9.0;
        let mut p = points(&truth(), &[30.3, 41.4, 48.7]);
        for point in &mut p.points {
            point.reference_c = round_f(point.reference_c);
        }
        let fit = fit(&p, "bath", R_FIXED, Terms::Two).unwrap();
        assert!(fit.thermistor.a > 0.0 && fit.thermistor.b > 0.0);
        assert!(
            fit.residuals.iter().all(|(_, _, r)| r.abs() < 0.3),
            "{:?}",
            fit.residuals
        );
        assert_eq!(Terms::for_points(3), Terms::Two);
        assert_eq!(Terms::for_points(4), Terms::Three);
    }

    #[test]
    fn points_round_trip_through_toml() {
        let p = points(&truth(), &[0.0, 50.0]);
        let text = toml::to_string(&p).unwrap();
        assert!(text.contains("[[point]]"));
        let back: Points = toml::from_str(&text).unwrap();
        assert_eq!(back, p);
    }
}
