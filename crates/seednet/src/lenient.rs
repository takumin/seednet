//! Deserializers for the scalars that cloud-init reads with PyYAML, which takes YAML 1.1 booleans
//! (`yes`, `on`) and accepts a number written as a string.

use serde::{Deserialize, Deserializer, de::Error};

#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar {
    Bool(bool),
    Int(u64),
    Str(String),
}

pub fn opt_bool<'de, D: Deserializer<'de>>(d: D) -> Result<Option<bool>, D::Error> {
    match Option::<Scalar>::deserialize(d)? {
        None => Ok(None),
        Some(Scalar::Bool(value)) => Ok(Some(value)),
        Some(Scalar::Int(value)) => Ok(Some(value != 0)),
        Some(Scalar::Str(value)) => match value.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "y" => Ok(Some(true)),
            "false" | "no" | "off" | "n" => Ok(Some(false)),
            _ => Err(D::Error::custom(format!("invalid boolean: {value}"))),
        },
    }
}

pub fn opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    match Option::<Scalar>::deserialize(d)? {
        None => Ok(None),
        Some(Scalar::Int(value)) => u32::try_from(value)
            .map(Some)
            .map_err(|_| D::Error::custom(format!("number out of range: {value}"))),
        Some(Scalar::Str(value)) => value
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| D::Error::custom(format!("invalid number: {value}"))),
        Some(Scalar::Bool(value)) => Err(D::Error::custom(format!("invalid number: {value}"))),
    }
}

/// A string, or a number such as a prefix length given as an integer.
pub fn opt_string<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    match Option::<Scalar>::deserialize(d)? {
        None => Ok(None),
        Some(Scalar::Str(value)) => Ok(Some(value)),
        Some(Scalar::Int(value)) => Ok(Some(value.to_string())),
        Some(Scalar::Bool(value)) => Err(D::Error::custom(format!("invalid string: {value}"))),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// A list, which cloud-init also takes as a single string, split on whitespace or commas.
pub fn list<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    Ok(match Option::<OneOrMany>::deserialize(d)? {
        None => Vec::new(),
        Some(OneOrMany::One(value)) => value
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
        Some(OneOrMany::Many(values)) => values,
    })
}

/// The keys of a mapping that its struct does not read, to warn about rather than drop unseen.
pub type Rest = std::collections::BTreeMap<String, yaml_serde::Value>;

/// Warns about each key in `rest`, of `what` such as `eth0` or `a route of eth0`.
pub fn warn_unknown(rest: &Rest, what: &str, warnings: &mut Vec<String>) {
    for key in rest.keys() {
        warnings.push(format!("skipping the unsupported key {key} of {what}"));
    }
}
