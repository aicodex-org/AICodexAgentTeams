//! 原生问题与概率合同校验，原始 JSON 独立保留供 wire 转发

use crate::provider::not_sent;
use base64::Engine as _;
use gateway_core::operation::RawJsonPayload;
use gateway_core::{
    error::{ProviderError, ProviderErrorKind},
    metering::Usage,
    upstream::UpstreamSendState,
};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

const MAX_REQUEST: usize = 32 * 1024 * 1024;
const MAX_DEPTH: usize = 128;
const TOLERANCE: f64 = 1e-6;

#[derive(Clone, Debug)]
pub(crate) enum Question {
    Noul,
    Choice(Vec<String>),
    Score(usize),
}
pub(crate) struct RequestShape {
    pub model: String,
    pub questions: BTreeMap<String, Question>,
}

// serde_json 的普通 Value 解析会覆盖重复键，先独立检查对象，避免校验与上游读取不同事实
struct Check(usize);
impl<'de> DeserializeSeed<'de> for Check {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        if self.0 > MAX_DEPTH {
            return Err(de::Error::custom("JSON nesting limit"));
        }
        deserializer.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Check {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("JSON value")
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<(), E> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(E::custom("nonfinite number"))
        }
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while seq.next_element_seed(Check(self.0 + 1))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        let mut seen = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key) {
                return Err(de::Error::custom("duplicate JSON key"));
            }
            map.next_value_seed(Check(self.0 + 1))?;
        }
        Ok(())
    }
}
pub(crate) fn parse_json(bytes: &[u8]) -> Result<Value, ()> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    Check(0).deserialize(&mut deserializer).map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ())?;
    fn finite(value: &Value) -> bool {
        match value {
            Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
            Value::Array(a) => a.iter().all(finite),
            Value::Object(o) => o.values().all(finite),
            _ => true,
        }
    }
    finite(&value).then_some(value).ok_or(())
}
fn description(value: &Value) -> bool {
    value.is_string() || value.is_object() || value.is_array()
}
fn field_depth(value: &Value, depth: usize) -> bool {
    match value {
        Value::Array(values) => depth < 64 && values.iter().all(|v| field_depth(v, depth + 1)),
        Value::Object(values) => depth < 64 && values.values().all(|v| field_depth(v, depth + 1)),
        _ => true,
    }
}
fn invalid() -> ProviderError {
    not_sent(ProviderErrorKind::InvalidRequest)
}
pub(crate) fn images_enabled(payload: &RawJsonPayload) -> bool {
    payload
        .context()
        .get("typesafe_images_enabled")
        .and_then(Value::as_bool)
        == Some(true)
}

pub(crate) fn validate_request(payload: &RawJsonPayload) -> Result<RequestShape, ProviderError> {
    let bytes = payload.body();
    if bytes.len() > MAX_REQUEST {
        return Err(not_sent(ProviderErrorKind::MessageTooBig));
    }
    let value = parse_json(bytes).map_err(|_| invalid())?;
    let object = value.as_object().ok_or_else(invalid)?;
    if object
        .get("stream")
        .is_some_and(|v| !v.is_null() && v != false)
        || ["tools", "previous_response_id"]
            .iter()
            .any(|key| object.get(*key).is_some_and(|v| !v.is_null()))
    {
        return Err(not_sent(ProviderErrorKind::Unsupported));
    }
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid)?;
    if !object
        .get("state")
        .is_some_and(|v| description(v) && field_depth(v, 0))
    {
        return Err(invalid());
    }
    let questions = object
        .get("questions")
        .and_then(Value::as_object)
        .filter(|o| !o.is_empty())
        .ok_or_else(invalid)?;
    validate_assets(&value, images_enabled(payload))?;
    let mut parsed = BTreeMap::new();
    for (id, question) in questions {
        let object = question.as_object().ok_or_else(invalid)?;
        if !object
            .get("instructions")
            .is_some_and(|v| description(v) && field_depth(v, 0))
        {
            return Err(invalid());
        }
        if object.get("criteria").is_some_and(|v| !field_depth(v, 0)) {
            return Err(invalid());
        }
        let question = match object.get("type").and_then(Value::as_str) {
            Some("noul") => {
                if let Some(criteria) = object.get("criteria").filter(|v| !v.is_null()) {
                    let criteria = criteria.as_object().ok_or_else(invalid)?;
                    if ["true", "false"].iter().any(|key| {
                        criteria
                            .get(*key)
                            .is_some_and(|v| !v.is_null() && !description(v))
                    }) {
                        return Err(invalid());
                    }
                }
                Question::Noul
            }
            Some("choice") => {
                let criteria = object
                    .get("criteria")
                    .and_then(Value::as_object)
                    .ok_or_else(invalid)?;
                if criteria.is_empty()
                    || criteria.len() > 255
                    || criteria.values().any(|v| !v.is_null() && !description(v))
                {
                    return Err(invalid());
                }
                Question::Choice(criteria.keys().cloned().collect())
            }
            Some("score") => {
                let criteria = object
                    .get("criteria")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?;
                if !(2..=10).contains(&criteria.len()) || !criteria.iter().all(description) {
                    return Err(invalid());
                }
                Question::Score(criteria.len())
            }
            Some("box") => return Err(not_sent(ProviderErrorKind::Unsupported)),
            _ => return Err(invalid()),
        };
        parsed.insert(id.clone(), question);
    }
    Ok(RequestShape {
        model: model.to_owned(),
        questions: parsed,
    })
}
fn probability(value: Option<&Value>) -> Result<f64, ()> {
    value
        .and_then(Value::as_f64)
        .filter(|p| p.is_finite() && (0.0..=1.0).contains(p))
        .ok_or(())
}
fn distribution(answer: &Value, keys: &[String]) -> Result<Vec<f64>, ()> {
    let object = answer
        .get("probabilities")
        .and_then(Value::as_object)
        .ok_or(())?;
    if object.len() != keys.len() {
        return Err(());
    }
    let values = keys
        .iter()
        .map(|key| probability(object.get(key)))
        .collect::<Result<Vec<_>, _>>()?;
    if (values.iter().sum::<f64>() - 1.0).abs() > TOLERANCE {
        return Err(());
    }
    Ok(values)
}
pub(crate) fn validate_response(
    bytes: &[u8],
    request: &RequestShape,
) -> Result<(String, Usage), ProviderError> {
    let validate = || -> Result<(String, Usage), ()> {
        let value = parse_json(bytes)?;
        let model = value
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or(())?;
        gateway_core::routing::UpstreamModelId::new(model).map_err(|_| ())?;
        let answers = value.get("answers").and_then(Value::as_object).ok_or(())?;
        for (id, question) in &request.questions {
            let answer = answers.get(id).filter(|v| v.is_object()).ok_or(())?;
            let typ = answer.get("type").and_then(Value::as_str).ok_or(())?;
            match question {
                Question::Noul if typ == "noul" => {
                    probability(answer.get("noul"))?;
                }
                Question::Choice(keys) if typ == "choice" => {
                    let choice = answer.get("choice").and_then(Value::as_str).ok_or(())?;
                    if !keys.iter().any(|key| key == choice) {
                        return Err(());
                    }
                    distribution(answer, keys)?;
                    probability(answer.get("confidence"))?;
                }
                Question::Score(count) if typ == "score" => {
                    let keys = (0..*count).map(|i| i.to_string()).collect::<Vec<_>>();
                    let probabilities = distribution(answer, &keys)?;
                    let score = answer
                        .get("score")
                        .and_then(Value::as_f64)
                        .filter(|n| n.is_finite() && *n >= 0.0 && *n <= (*count - 1) as f64)
                        .ok_or(())?;
                    let expected = probabilities
                        .iter()
                        .enumerate()
                        .map(|(i, p)| i as f64 * p)
                        .sum::<f64>();
                    if (score - expected).abs() > TOLERANCE {
                        return Err(());
                    }
                    let legend = answer.get("legend").and_then(Value::as_object).ok_or(())?;
                    if legend.len() != *count
                        || keys
                            .iter()
                            .any(|key| !legend.get(key).is_some_and(description))
                    {
                        return Err(());
                    }
                    probability(answer.get("confidence"))?;
                }
                _ => return Err(()),
            }
        }
        let mut usage = Usage::new();
        if let Some(raw) = value.get("usage").filter(|v| !v.is_null()) {
            let raw = raw.as_object().ok_or(())?;
            let tokens = |key| -> Result<Option<u64>, ()> {
                match raw.get(key) {
                    None | Some(Value::Null) => Ok(None),
                    Some(v) => v.as_u64().map(Some).ok_or(()),
                }
            };
            usage.input_tokens = tokens("input_tokens")?;
            usage.output_tokens = tokens("output_tokens")?;
        }
        Ok((model.to_owned(), usage))
    };
    validate().map_err(|_| {
        ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
            .with_retry_prohibited()
    })
}

// 这里只验证 JSON 图片封套与解码后的文件上限，不改写图片字节
// 像素尺寸、动画、文件损坏和引用展开后的计量由上游执行其原生校验
fn validate_assets(document: &Value, enabled: bool) -> Result<(), ProviderError> {
    let Some(assets) = document.get("assets").filter(|v| !v.is_null()) else {
        return Ok(());
    };
    let assets = assets.as_object().ok_or_else(invalid)?;
    if !assets.is_empty() && !enabled {
        return Err(invalid());
    }
    let mut reference_bytes = 0usize;
    for (reference, asset) in assets {
        reference_bytes += reference.len();
        if reference.len() > 1024 || reference_bytes > 65536 {
            return Err(not_sent(ProviderErrorKind::MessageTooBig));
        }
        if reference.is_empty() {
            return Err(invalid());
        }
        let asset = asset.as_object().ok_or_else(invalid)?;
        if asset.get("type").is_some_and(|v| v != "image") || asset.contains_key("part") {
            return Err(invalid());
        }
        let format = asset
            .get("format")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if !matches!(format, "png" | "jpg" | "jpeg") {
            return Err(invalid());
        }
        let encoded = asset
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        if encoded.len() > 16_777_216usize.div_ceil(3) * 4 {
            return Err(not_sent(ProviderErrorKind::MessageTooBig));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| invalid())?;
        if decoded.len() > 16 * 1024 * 1024 {
            return Err(not_sent(ProviderErrorKind::MessageTooBig));
        }
        if decoded.is_empty() {
            return Err(invalid());
        }
        let referenced = document
            .get("state")
            .is_some_and(|v| references_asset(v, reference))
            || document
                .get("questions")
                .and_then(Value::as_object)
                .is_some_and(|questions| {
                    questions.values().any(|q| {
                        ["instructions", "criteria"].iter().any(|field| {
                            q.get(field).is_some_and(|v| references_asset(v, reference))
                        })
                    })
                });
        if !referenced {
            return Err(invalid());
        }
    }
    Ok(())
}

// 官方按字符串值中的引用匹配，JSON 键不参与图片替换
fn references_asset(value: &Value, reference: &str) -> bool {
    match value {
        Value::String(text) => text.contains(reference),
        Value::Array(values) => values.iter().any(|v| references_asset(v, reference)),
        Value::Object(values) => values.values().any(|v| references_asset(v, reference)),
        _ => false,
    }
}
