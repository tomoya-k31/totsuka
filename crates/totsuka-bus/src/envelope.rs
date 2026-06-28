use crate::pgmq::BusError;
use serde_json::Value;
use totsuka_core::EventEnvelope;

pub fn envelope_to_json(env: &EventEnvelope) -> Result<Value, BusError> {
    Ok(serde_json::to_value(env)?)
}

pub fn json_to_envelope(v: Value) -> Result<EventEnvelope, BusError> {
    Ok(serde_json::from_value(v)?)
}
