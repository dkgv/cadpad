//! Concrete states: a [`StateSpace`] expanded into the exact states a plan
//! runs, in a fixed order. Sampling is explicit: `a, a+s, ..., b` with `b`
//! always included.
use super::ir::{AxisRef, JointRef, Literal, ParamRef, ParamValues, StateSpace, Sweep, Target};
use super::quantity::Quantity;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One coordinate of a state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StateValue {
    Param {
        param: ParamRef,
        value: Quantity,
    },
    Rotate {
        moving: Target,
        axis: AxisRef,
        angle: Quantity,
    },
    Translate {
        moving: Target,
        direction: AxisRef,
        distance: Quantity,
    },
    Joint {
        joint: JointRef,
        value: Quantity,
    },
}

/// A point of a state space. `rest` is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConcreteState {
    pub values: Vec<StateValue>,
}

/// Round to 12 significant digits, so `0.1 * 3` prints as `0.3`. Deterministic.
fn clean(v: f64) -> f64 {
    if v == 0.0 || !v.is_finite() {
        return v;
    }
    format!("{v:.11e}").parse().unwrap_or(v)
}

/// Values `from, from+step, ..., to` in the unit of `from`. Empty when the
/// sweep is malformed (see `StateSpace::validate`).
pub fn sweep_values(from: &Literal, to: &Literal, step: &Literal) -> Vec<Quantity> {
    let unit = from.unit;
    let conv = |l: &Literal| l.quantity().convert_to(unit).map(|q| q.value);
    let (Ok(b), Ok(s)) = (conv(to), conv(step)) else {
        return vec![];
    };
    let a = from.value;
    if !(a.is_finite() && b.is_finite() && s.is_finite()) || s <= 0.0 || b < a {
        return vec![];
    }
    let steps = (b - a) / s;
    if steps > 1e7 {
        return vec![];
    }
    let whole = (steps + 1e-9).floor();
    let mut out: Vec<Quantity> = (0..=whole as u64)
        .map(|i| Quantity::new(clean(a + i as f64 * s), unit))
        .collect();
    if (steps - whole).abs() > 1e-9 {
        out.push(Quantity::new(b, unit));
    } else if let Some(last) = out.last_mut() {
        last.value = clean(b); // `to` exactly
    }
    out
}

fn sweep_of(range: &Sweep) -> Vec<Quantity> {
    sweep_values(&range.from, &range.to, &range.step)
}

/// Expand a state space. Product factors vary last-factor-fastest.
pub fn expand_states(space: &StateSpace) -> Vec<ConcreteState> {
    let one = |v: StateValue| ConcreteState { values: vec![v] };
    match space {
        StateSpace::Rest {} => vec![ConcreteState::default()],
        StateSpace::Param { param, values } => {
            let qs: Vec<Quantity> = match values {
                ParamValues::Value { value } => vec![value.quantity()],
                ParamValues::Range { from, to, step } => sweep_values(from, to, step),
                ParamValues::List { values } => values.iter().map(Literal::quantity).collect(),
            };
            qs.into_iter()
                .map(|value| {
                    one(StateValue::Param {
                        param: param.clone(),
                        value,
                    })
                })
                .collect()
        }
        StateSpace::Rotate {
            moving,
            axis,
            range,
        } => sweep_of(range)
            .into_iter()
            .map(|angle| {
                one(StateValue::Rotate {
                    moving: moving.clone(),
                    axis: axis.clone(),
                    angle,
                })
            })
            .collect(),
        StateSpace::Translate {
            moving,
            direction,
            range,
        } => sweep_of(range)
            .into_iter()
            .map(|distance| {
                one(StateValue::Translate {
                    moving: moving.clone(),
                    direction: direction.clone(),
                    distance,
                })
            })
            .collect(),
        StateSpace::Joint { joint, range } => sweep_of(range)
            .into_iter()
            .map(|value| {
                one(StateValue::Joint {
                    joint: joint.clone(),
                    value,
                })
            })
            .collect(),
        StateSpace::Product { factors } => {
            let mut acc = vec![ConcreteState::default()];
            for f in factors {
                let next = expand_states(f);
                acc = acc
                    .iter()
                    .flat_map(|a| {
                        next.iter().map(move |n| ConcreteState {
                            values: a.values.iter().chain(&n.values).cloned().collect(),
                        })
                    })
                    .collect();
            }
            acc
        }
    }
}

impl ConcreteState {
    /// True when every coordinate of `pattern` is also in `self`: how an
    /// allowance limited to `door_angle = 0` matches a plan state.
    pub fn contains(&self, pattern: &ConcreteState) -> bool {
        pattern.values.iter().all(|p| {
            self.values.iter().any(|v| match (v, p) {
                (
                    StateValue::Param { param: a, value: x },
                    StateValue::Param { param: b, value: y },
                ) => a.name == b.name && x.same_value(y),
                (
                    StateValue::Joint { joint: a, value: x },
                    StateValue::Joint { joint: b, value: y },
                ) => a.name == b.name && x.same_value(y),
                _ => v == p,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::Unit;

    fn lit(v: f64, u: Unit) -> Literal {
        Literal::new(v, u)
    }
    fn range(a: f64, b: f64, s: f64) -> StateSpace {
        StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "p".into(),
            },
            values: ParamValues::Range {
                from: lit(a, Unit::Deg),
                to: lit(b, Unit::Deg),
                step: lit(s, Unit::Deg),
            },
        }
    }
    fn vals(s: &StateSpace) -> Vec<f64> {
        expand_states(s)
            .iter()
            .map(|c| match &c.values[0] {
                StateValue::Param { value, .. } => value.value,
                _ => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn ranges_include_the_end_and_match_state_count() {
        assert_eq!(vals(&range(0.0, 10.0, 5.0)), [0.0, 5.0, 10.0]);
        assert_eq!(vals(&range(0.0, 11.0, 5.0)), [0.0, 5.0, 10.0, 11.0]);
        assert_eq!(vals(&range(0.0, 1.0, 0.1)).len(), 11);
        assert_eq!(vals(&range(0.0, 0.3, 0.1)), [0.0, 0.1, 0.2, 0.3]);
        for s in [
            range(0.0, 110.0, 2.0),
            range(48.0, 132.0, 2.0),
            range(0.0, 360.0, 5.0),
            range(0.0, 7.0, 3.0),
        ] {
            assert_eq!(expand_states(&s).len() as u64, s.state_count().unwrap());
        }
        assert!(vals(&range(0.0, 1.0, 0.0)).is_empty());
    }

    #[test]
    fn mixed_units_convert_to_the_start_unit() {
        let s = StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "p".into(),
            },
            values: ParamValues::Range {
                from: lit(0.0, Unit::Mm),
                to: lit(1.0, Unit::Cm),
                step: lit(5.0, Unit::Mm),
            },
        };
        assert_eq!(vals(&s), [0.0, 5.0, 10.0]);
    }

    #[test]
    fn products_vary_the_last_factor_fastest() {
        let other = StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "q".into(),
            },
            values: ParamValues::List {
                values: vec![lit(1.0, Unit::Mm), lit(2.0, Unit::Mm)],
            },
        };
        let states = expand_states(&StateSpace::Product {
            factors: vec![range(0.0, 10.0, 5.0), other],
        });
        assert_eq!(states.len(), 6);
        let pq: Vec<(f64, f64)> = states
            .iter()
            .map(|s| match (&s.values[0], &s.values[1]) {
                (StateValue::Param { value: a, .. }, StateValue::Param { value: b, .. }) => {
                    (a.value, b.value)
                }
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(pq[..3], [(0.0, 1.0), (0.0, 2.0), (5.0, 1.0)]);
    }

    #[test]
    fn rest_is_one_empty_state_and_patterns_match_by_coordinates() {
        assert_eq!(
            expand_states(&StateSpace::Rest {}),
            vec![ConcreteState::default()]
        );
        let states = expand_states(&range(0.0, 10.0, 5.0));
        let zero = &expand_states(&StateSpace::Param {
            param: ParamRef {
                model: None,
                name: "p".into(),
            },
            values: ParamValues::Value {
                value: lit(0.0, Unit::Rad),
            },
        })[0];
        assert!(states[0].contains(zero));
        assert!(!states[1].contains(zero));
        assert!(!ConcreteState::default().contains(zero));
        assert!(states[1].contains(&ConcreteState::default()));
    }
}
