use anyhow::Result;
use serde_json::Value;

pub struct ClientHelloMessage;

impl ClientHelloMessage {
    pub fn new() -> Result<String> {
        let body = r#"{
    "type": "hello",
    "version": 1,
    "transport": "websocket",
    "features": {
        "mcp": true
    },
    "audio_params": {
        "format": "opus",
        "sample_rate": 16000,
        "channels": 1,
        "frame_duration": 60
    }
}"#;
        let hello: Value = serde_json::from_str(body)?;
        // hello["feature"] = json!({ "an": "object" });
        println!("{:?}", hello);
        println!("{:?}", serde_json::to_string(&hello));
        Ok(serde_json::to_string(&hello)?)
    }
}
