use std::ffi::CStr;

use esp_idf_svc::eventloop::{
    EspEvent, EspEventDeserializer, EspEventPostData, EspEventSerializer, EspEventSource,
};

use crate::audio::codec::AudioStreamPacket;

pub const WEBSOCKET_PROTOCOL_SERVER_HELLO_EVENT: u32 = 1;

#[derive(Copy, Clone, Debug)]
pub enum WsEvent {
    // Start,
    WebSocketConnected,
    ServerHelloMessageReceived, // 收到服务器返回的hello消息
    SendAudioEvent,             // 发送音频数据事件
                                // Tick(u32),
}
unsafe impl EspEventSource for WsEvent {
    #[allow(clippy::manual_c_str_literals)]
    fn source() -> Option<&'static CStr> {
        // String should be unique across the whole project and ESP IDF
        Some(CStr::from_bytes_with_nul(b"DEMO-SERVICE\0").unwrap())
    }
}

impl EspEventSerializer for WsEvent {
    type Data<'a> = WsEvent;

    fn serialize<F, R>(event: &Self::Data<'_>, f: F) -> R
    where
        F: FnOnce(&EspEventPostData) -> R,
    {
        // Go the easy way since our payload implements Copy and is `'static`
        f(&unsafe { EspEventPostData::new(Self::source().unwrap(), Self::event_id(), event) })
    }
}

impl EspEventDeserializer for WsEvent {
    type Data<'a> = WsEvent;

    fn deserialize<'a>(data: &EspEvent<'a>) -> Self::Data<'a> {
        // Just as easy as serializing
        *unsafe { data.as_payload::<WsEvent>() }
    }
}

#[derive(Clone, Debug)]
pub enum XzEvent {
    BootButtonClicked,
    VolumeButtonClicked,
    OpenAudioChannel,
    CloseAudioChannel,
    WebSocketConnected,
    ServerHelloMessageReceived(String), // 收到服务器返回的hello消息
    AddAudioPacketToQueue(AudioStreamPacket), //add encoded audio packet to the wait-for-sending queue
    SendAudioEvent,                           // 发送音频数据事件
    AudioDataReceived(AudioStreamPacket),
    WebsocketTextMessageReceived(String),
}
