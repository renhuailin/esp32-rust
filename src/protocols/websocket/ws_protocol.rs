use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Error, Result};
use esp_idf_hal::io::EspIOError;
use esp_idf_svc::ws::client::{
    EspWebSocketClient, EspWebSocketClientConfig, WebSocketEvent, WebSocketEventType,
};
use esp_idf_svc::ws::FrameType;
use esp_idf_sys::EspError;
use log::{error, info};

use crate::audio::AudioStreamPacket;
use crate::common::enums::AbortReason;
use crate::common::event::XzEvent;
use crate::protocols::protocol::Protocol;
use crate::protocols::websocket::message::ClientHelloMessage;

pub struct WebSocketProtocol {
    client: Option<Box<EspWebSocketClient<'static>>>,
    // sender: Sender<XzEvent>,
    internal_sender: Sender<XzEvent>,
    internal_receiver: Receiver<XzEvent>,
    // 不再存储 config，而是存储构建 config 所需的数据
    device_id: String,
    is_connected: bool,
    // session_id: Option<String>, //Websocket协议不返回 session_id，所以消息中的会话ID可设置为空。
    on_incoming_text: Option<Box<dyn FnMut(&str) -> Result<(), Error> + Send + 'static>>,
    on_incoming_audio:
        Option<Box<dyn FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static>>,
    last_incoming_time: Arc<Mutex<Option<Instant>>>, // 上一次收到服务器端数据的时间
}

impl WebSocketProtocol {
    /// 创建一个新的 WebSocketProtocol 实例
    ///
    /// # 参数
    /// * `device_id` - 设备标识符字符串引用
    /// * `sender` - 用于发送 XzEvent 事件的 Sender 通道，WebSocket收到服务器端发过来的数据时，
    /// 将数据封装成 XzEvent，发送给 XzEvent 处理模块，也就是添加到主线程中队列中。
    ///
    /// # 返回值
    /// 返回初始化后的 WebSocketProtocol 实例
    pub fn new(device_id: &str) -> Self {
        let (inner_sender, inner_receiver): (Sender<XzEvent>, Receiver<XzEvent>) = channel();
        Self {
            client: None,
            // sender,
            device_id: device_id.to_string(),
            is_connected: false,
            internal_sender: inner_sender,
            internal_receiver: inner_receiver,
            on_incoming_text: None,
            on_incoming_audio: None,
            last_incoming_time: Arc::new(Mutex::new(None)),
        }
    }

    pub fn is_connected(&self) -> bool {
        if let Some(client) = &self.client {
            return client.is_connected() && self.is_connected;
        }
        false
    }

    pub fn get_last_incoming_time(&self) -> Option<Instant> {
        *self.last_incoming_time.lock().unwrap()
    }

    pub fn send_hello_message(&mut self) -> Result<()> {
        info!("try to send client  hello message to server.");
        let message = ClientHelloMessage::new()?;
        if let Some(client) = &mut self.client {
            match client.send(FrameType::Text(false), message.as_bytes()) {
                Ok(_) => {}
                Err(e) => {
                    error!("Error sending audio data: {:?}", e);
                }
            }
        }
        Ok(())
    }

    // 当收到服务器端的 hello message 时，才认为连接成功。
    pub fn on_server_hello_msg(&mut self) {
        self.is_connected = true;
        // self.internal_sender
        //     .send(XzEvent::ServerHelloMessageReceived)
        //     .unwrap();
    }

    pub fn send(&mut self, frame_type: FrameType, frame_data: &[u8]) -> Result<(), EspError> {
        if self.is_connected() {
            if let Some(client) = &mut self.client {
                match client.send(frame_type, frame_data) {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Error sending audio data: {:?}", e);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn close_audio_channel(&mut self) -> Result<(), Error> {
        if let Some(_) = self.client.take() {
            // client.close().unwrap();
        }
        self.is_connected = false;
        *self.last_incoming_time.lock().unwrap() = None;
        Ok(())
    }
}

impl Protocol for WebSocketProtocol {
    fn send_text(&mut self, text: &str) -> Result<()> {
        todo!()
    }

    fn send_audio(&mut self, audio: &AudioStreamPacket) -> Result<()> {
        todo!()
    }

    fn open_audio_channel(&mut self) -> Result<bool, Error> {
        if self.client.is_some() {
            info!("Audio channel already opened,so closing it first");
            self.close_audio_channel()?;
        }

        let header = format!(
            "Protocol-Version: 1\r\ndevice-id: {}\r\nClient-Id: {}\r\n",
            self.device_id, self.device_id
        );

        let timeout = Duration::from_secs(10);
        // let (tx, rx) = mpsc::channel::<ExampleEvent>();
        // let ws_url = "ws://192.168.1.231:8000/xiaozhi/v1/";
        let ws_url = "ws://192.168.1.105:8000/xiaozhi/v1/";

        let config = EspWebSocketClientConfig {
            headers: Some(header.as_str()),
            ..Default::default()
        };

        // let sender = self.sender.clone();

        info!("Connecting to {}", ws_url);
        // let internal_sender1 = self.internal_sender.clone();
        let last_incoming_time = self.last_incoming_time.clone();

        let mut on_incoming_text_handler = self.on_incoming_text.take();
        let mut on_incoming_audio_handler = self.on_incoming_audio.take();

        let inner_sender = self.internal_sender.clone();
        self.client = Some(Box::new(EspWebSocketClient::new(
            ws_url,
            &config,
            timeout,
            move |event| {
                // info!("handle event");
                // handle_event(event, internal_sender1.clone(), sender.clone());
                if let Ok(event) = event {
                    match event.event_type {
                        WebSocketEventType::BeforeConnect => {
                            info!("Websocket before connect");
                        }
                        WebSocketEventType::Connected => {
                            info!("Websocket connected");
                            // internal_sender.send(XzEvent::WebSocketConnected).unwrap();
                        }
                        WebSocketEventType::Disconnected => {
                            info!("Websocket disconnected");
                        }

                        WebSocketEventType::Close(reason) => {
                            info!("Websocket close, reason: {reason:?}");
                        }

                        WebSocketEventType::Closed => {
                            info!("Websocket closed");
                        }

                        WebSocketEventType::Text(text) => {
                            info!("Websocket received a text message, text: {text}");
                            let message: serde_json::Value = serde_json::from_str(text).unwrap();
                            info!("parse json success");
                            if let Some(message_type) = message["type"].as_str() {
                                if message_type == "hello" {
                                    inner_sender
                                        .send(XzEvent::ServerHelloMessageReceived(text.to_string()))
                                        .unwrap();
                                } else {
                                    if let Some(on_incoming_text) =
                                        on_incoming_text_handler.as_mut()
                                    {
                                        let _ = (on_incoming_text)(text);
                                    }
                                }
                            }

                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                        }

                        WebSocketEventType::Binary(binary) => {
                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                            // info!("Websocket recv, binary: {binary:?}");
                            let packet = AudioStreamPacket {
                                sample_rate: 16000,
                                frame_duration: 60,
                                timestamp: 0,
                                payload: binary.to_vec(),
                            };

                            // sender.send(XzEvent::AudioDataReceived(packet)).unwrap();

                            if let Some(on_incoming_audio) = on_incoming_audio_handler.as_mut() {
                                let _ = (on_incoming_audio)(&packet);
                            }
                        }
                        WebSocketEventType::Ping => {
                            info!("Websocket ping");
                        }
                        WebSocketEventType::Pong => {
                            info!("Websocket pong");
                        }
                    }
                }
            },
        )?));

        info!("wait for server hello message");
        // wait for server hello message
        // cmd_receiver.recv()?;
        for event in &self.internal_receiver {
            match event {
                XzEvent::WebSocketConnected => {
                    info!("Connected,try to send hello message");
                    // send client hello message
                    if let Some(client) = &mut self.client {
                        if client.is_connected() {
                            let hello_message = ClientHelloMessage::new().unwrap();
                            info!("Worker thread: Sending hello message...");
                            match client.send(FrameType::Text(false), hello_message.as_bytes()) {
                                Ok(_) => info!("Worker thread: Hello message sent!"),
                                Err(e) => info!("Worker thread: Send error: {:?}", e),
                            }
                        } else {
                            info!("Worker thread: Client not connected, cannot send.");
                        }
                    }
                    // break;
                }
                XzEvent::ServerHelloMessageReceived(_) => {
                    // info!("Worker thread: Server hello message received. {}", text);
                    // self.parse_server_hello_message(text);
                    break;
                }
                // XzEvent::SendAudioEvent => todo!(),
                // XzEvent::AudioDataReceived(audio_stream_packet) => todo!(),
                _ => todo!(),
            }
        }

        info!("ws protocol is connected.");
        self.is_connected = true;

        Ok(true)
    }

    fn on_incoming_text<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&str) -> Result<(), Error> + Send + 'static,
    {
        self.on_incoming_text = Some(Box::new(handler));
        Ok(())
    }

    fn on_incoming_audio<F>(&mut self, handler: F) -> Result<(), Error>
    where
        F: FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static,
    {
        self.on_incoming_audio = Some(Box::new(handler));
        Ok(())
    }

    fn is_timeout(&self) -> bool {
        let new_now = Instant::now();
        let timeout_seconds = 120;
        if let Some(last_incoming_time) = *self.last_incoming_time.lock().unwrap() {
            if new_now.duration_since(last_incoming_time).as_secs() > timeout_seconds {
                return true;
            }
        }
        return false;
    }

    fn is_audio_channel_opened(&self) -> bool {
        // return websocket_ != nullptr && websocket_->IsConnected() && !error_occurred_ && !IsTimeout();
        return self.is_connected && !self.is_timeout();
    }

    fn send_abort_speaking(&mut self, reason: AbortReason) -> Result<(), Error> {
        let message = match reason {
            AbortReason::WakeWordDetected => {
                format!(
                    r#"{{"session_id":"{}","type":"abort","reason":"wake_word_detected"}}"#,
                    self.device_id
                )
            }
            _ => {
                format!(r#"{{"session_id":"{}","type":"abort"}}"#, self.device_id)
            }
        };
        self.send_text(&message)?;

        //     std::string message = "{\"session_id\":\"" + session_id_ + "\",\"type\":\"abort\"";
        // if (reason == kAbortReasonWakeWordDetected) {
        //     message += ",\"reason\":\"wake_word_detected\"";
        // }
        // message += "}";
        // SendText(message);
        // let message = format!(r#"{{"session_id":"{}","type":"abort""#, self.session_id);

        Ok(())
    }
}

impl Drop for WebSocketProtocol {
    fn drop(&mut self) {
        let _ = self.close_audio_channel();
    }
}

fn handle_event(
    event: &Result<WebSocketEvent, EspIOError>,
    internal_sender: Sender<XzEvent>,
    sender: Sender<XzEvent>,
) {
    if let Ok(event) = event {
        match event.event_type {
            WebSocketEventType::BeforeConnect => {
                info!("Websocket before connect");
            }
            WebSocketEventType::Connected => {
                info!("Websocket connected");
                internal_sender.send(XzEvent::WebSocketConnected).unwrap();
                // tx.send(ExampleEvent::Connected).ok();
                // sys_loop
                //     .post::<CustomEvent>(&CustomEvent::WebSocketConnected, delay::BLOCK)
                //     .unwrap();
            }
            WebSocketEventType::Disconnected => {
                info!("Websocket disconnected");
            }

            WebSocketEventType::Close(reason) => {
                info!("Websocket close, reason: {reason:?}");
            }

            WebSocketEventType::Closed => {
                info!("Websocket closed");
            }

            WebSocketEventType::Text(text) => {
                // info!("Websocket received a text message");
                info!("Websocket received a text message, text: {text}");
                sender
                    .send(XzEvent::WebsocketTextMessageReceived(text.to_string()))
                    .unwrap();
                // let hello: serde_json::Value = serde_json::from_str(text).unwrap();
                // info!("parse json success");
            }

            WebSocketEventType::Binary(binary) => {
                // info!("Websocket recv, binary: {binary:?}");
                let packet = AudioStreamPacket {
                    sample_rate: 16000,
                    frame_duration: 60,
                    timestamp: 0,
                    payload: binary.to_vec(),
                };

                sender.send(XzEvent::AudioDataReceived(packet)).unwrap();
            }
            WebSocketEventType::Ping => {
                info!("Websocket ping");
            }
            WebSocketEventType::Pong => {
                info!("Websocket pong");
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum WebSocketHelloEvent {
    ServerHelloEvent,
}
