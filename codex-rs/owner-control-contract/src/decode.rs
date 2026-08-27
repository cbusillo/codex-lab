use serde::de::DeserializeOwned;
use serde::de::IntoDeserializer;
use serde_json::Value;
use serde_path_to_error::Segment;

use crate::ErrorLocation;
use crate::ValidationError;

pub(crate) fn deserialize_value<T: DeserializeOwned>(value: Value) -> Result<T, ValidationError> {
    serde_path_to_error::deserialize(value.into_deserializer()).map_err(|error| {
        let mut location = error
            .path()
            .iter()
            .filter_map(|segment| match segment {
                Segment::Seq { index } => Some(ErrorLocation::Index(*index)),
                Segment::Map { key } | Segment::Enum { variant: key } => {
                    Some(ErrorLocation::Field(key.clone()))
                }
                Segment::Unknown => None,
            })
            .collect::<Vec<_>>();
        let source = error.into_inner();
        append_message_field(&mut location, &source.to_string());
        ValidationError::Json { location, source }
    })
}

fn append_message_field(location: &mut Vec<ErrorLocation>, message: &str) {
    for prefix in ["missing field `", "unknown field `"] {
        let Some(remainder) = message.strip_prefix(prefix) else {
            continue;
        };
        let Some((field, _)) = remainder.split_once('`') else {
            continue;
        };
        if !matches!(location.last(), Some(ErrorLocation::Field(existing)) if existing == field) {
            location.push(ErrorLocation::Field(field.to_owned()));
        }
        return;
    }
}
