use std::ffi::CStr;

use esp_idf_svc::eventloop::{
    EspEvent, EspEventDeserializer, EspEventPostData, EspEventSerializer, EspEventSource,
};

pub const WEBSOCKET_PROTOCOL_SERVER_HELLO_EVENT: u32 = 1;

#[derive(Copy, Clone, Debug)]
pub enum CustomEvent {
    Start,
    WebSocketConnected,
    ServerHelloMessageReceived, // 收到服务器返回的hello消息
    Tick(u32),
}
unsafe impl EspEventSource for CustomEvent {
    #[allow(clippy::manual_c_str_literals)]
    fn source() -> Option<&'static CStr> {
        // String should be unique across the whole project and ESP IDF
        Some(CStr::from_bytes_with_nul(b"DEMO-SERVICE\0").unwrap())
    }
}

impl EspEventSerializer for CustomEvent {
    type Data<'a> = CustomEvent;

    fn serialize<F, R>(event: &Self::Data<'_>, f: F) -> R
    where
        F: FnOnce(&EspEventPostData) -> R,
    {
        // Go the easy way since our payload implements Copy and is `'static`
        f(&unsafe { EspEventPostData::new(Self::source().unwrap(), Self::event_id(), event) })
    }
}

impl EspEventDeserializer for CustomEvent {
    type Data<'a> = CustomEvent;

    fn deserialize<'a>(data: &EspEvent<'a>) -> Self::Data<'a> {
        // Just as easy as serializing
        *unsafe { data.as_payload::<CustomEvent>() }
    }
}
