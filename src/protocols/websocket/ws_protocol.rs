use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Error, Result};
use esp_idf_svc::ws::client::{EspWebSocketClient, EspWebSocketClientConfig, WebSocketEventType};
use esp_idf_svc::ws::FrameType;
use esp_idf_sys::EspError;
use log::{debug, error, info};

use crate::audio::codec::types::AudioStreamPacket;
use crate::common::enums::{AbortReason, ListeningMode};
use crate::common::event::AppEvent;
use crate::protocols::protocol::Protocol;
use crate::protocols::websocket::message::ClientHelloMessage;

pub struct WebSocketProtocol {
    client: Option<Box<EspWebSocketClient<'static>>>,
    // internal_sender: Option<Sender<AppEvent>>,
    // internal_receiver: Option<Receiver<AppEvent>>,
    external_sender: Sender<AppEvent>,
    // 不再存储 config，而是存储构建 config 所需的数据
    device_id: String,
    is_connected: bool,
    // session_id: Option<String>, //Websocket协议不返回 session_id，所以消息中的会话ID可设置为空。
    on_incoming_text: Option<Box<dyn FnMut(&str) -> Result<(), Error> + Send + 'static>>,
    on_incoming_audio:
        Option<Box<dyn FnMut(&AudioStreamPacket) -> Result<(), Error> + Send + 'static>>,
    on_network_error: Option<Box<dyn FnMut(&str) -> Result<(), Error> + Send + 'static>>,
    last_incoming_time: Arc<Mutex<Option<Instant>>>, // 上一次收到服务器端数据的时间
    server_hello_received: Arc<Mutex<bool>>,
    /// 连接世代计数：每次建立/关闭连接时递增。
    /// 用于丢弃旧连接销毁过程中迟到的 Disconnected/Closed 事件，
    /// 避免其把新会话的状态打回 Idle。
    conn_epoch: Arc<AtomicU32>,
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
    pub fn new(device_id: &str, sender: Sender<AppEvent>) -> Self {
        Self {
            client: None,
            // sender,
            device_id: device_id.to_string(),
            is_connected: false,
            // internal_sender: None,
            // internal_receiver: None,
            external_sender: sender,
            on_incoming_text: None,
            on_incoming_audio: None,
            last_incoming_time: Arc::new(Mutex::new(None)),
            server_hello_received: Arc::new(Mutex::new(false)),
            on_network_error: None,
            conn_epoch: Arc::new(AtomicU32::new(0)),
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

    #[allow(dead_code)]
    pub fn set_error(&mut self, error: &str) {
        let _ = self.on_network_error.as_mut().unwrap()(error);
    }
}

impl Protocol for WebSocketProtocol {
    fn send_text(&mut self, text: &str) -> Result<()> {
        if let Some(client) = &mut self.client {
            if client.is_connected() {
                info!("WebSocketProtocol: Sending text message - {} ", text);
                match client.send(FrameType::Text(false), text.as_bytes()) {
                    Ok(_) => info!("WebSocketProtocol: Hello message sent!"),
                    Err(e) => {
                        // 对齐 C++ 参考实现：文本发送失败只记录日志，不升级为网络错误。
                        // 旧行为：set_error -> ProtocolNetworkError -> 主循环强制切 Idle，
                        // 典型表现是"我在呢"播放完后（tts stop -> Listening 需要重发
                        // listen start），恰逢发送瞬时失败，设备直接回 Idle 而非 Listening。
                        // 连接真实断开由 websocket 的 Disconnected/Closed 事件感知。
                        error!("WebSocketProtocol: Send text error: {:?}", e);
                    }
                }
            } else {
                info!("WebSocketProtocol: Client not connected, cannot send.");
            }
        }
        Ok(())
    }

    fn send_audio(&mut self, packet: &AudioStreamPacket) -> Result<()> {
        if let Some(client) = &mut self.client {
            if client.is_connected() {
                match client.send(FrameType::Binary(false), &packet.payload) {
                    Ok(_) => {
                        // info!(
                        //     "WebSocketProtocol: Audio packet sent! {:?}",
                        //     &packet.payload
                        // )
                    }
                    Err(e) => info!("WebSocketProtocol: Send error: {:?}", e),
                }
            } else {
                info!("WebSocketProtocol : Client not connected, cannot send.");
            }
        }
        Ok(())
    }

    fn open_audio_channel(&mut self) -> Result<bool, Error> {
        if self.is_audio_channel_opened() && self.client.is_some() {
            info!("Audio channel already opened,so closing it first");
            match self.close_audio_channel() {
                Ok(_) => {}
                Err(e) => {
                    error!("Error closing audio channel: {:?}", e);
                }
            }
        }

        self.is_connected = false;

        let header = format!(
            "Protocol-Version: 1\r\ndevice-id: {}\r\nClient-Id: {}\r\n",
            self.device_id, self.device_id
        );
        info!("Web socket header: {}", header);

        let timeout = Duration::from_secs(10);

        let ws_url = "ws://192.168.3.5:8000/xiaozhi/v1/";
        // let ws_url = "ws://xiaogu.long9.net:8000/xiaozhi/v1/"; //阿里云上的服务

        let config = EspWebSocketClientConfig {
            headers: Some(header.as_str()),
            // 关闭后台自动重连：连接失败时 IDF 默认会每 10s 无限重试，
            // client drop（esp_websocket_client_destroy）要等重连定时器
            // 到期才能停任务，曾把 open_audio_channel 的失败路径拖住 15s。
            // 失败重连交给应用层（下次唤醒重新 open）。
            disable_auto_reconnect: true,
            // 单次连接尝试上限（IDF 默认 10s），不可达地址时更快失败
            network_timeout_ms: Duration::from_secs(5),
            ..Default::default()
        };

        info!("Connecting to {}", ws_url);

        let last_incoming_time = self.last_incoming_time.clone();

        // 连接世代 +1 并捕获到本次回调闭包中：
        // 只有世代仍为最新时，Disconnected/Close/Closed 事件才会上报，
        // 从而丢弃旧连接销毁时迟到的断开事件（否则会把新会话打回 Idle）。
        let conn_epoch = Arc::clone(&self.conn_epoch);
        let my_epoch = conn_epoch.fetch_add(1, Ordering::SeqCst) + 1;

        // let mut on_incoming_text_handler = self.on_incoming_text.take();
        // let mut on_incoming_audio_handler = self.on_incoming_audio.take();

        let (inner_sender, inner_receiver): (Sender<AppEvent>, Receiver<AppEvent>) = channel();

        // let inner_sender = self.internal_sender.clone();
        let external_sender = self.external_sender.clone();

        *self.server_hello_received.lock().unwrap() = false;
        let server_hello_received = self.server_hello_received.clone();

        self.client = Some(Box::new(EspWebSocketClient::new(
            ws_url,
            &config,
            timeout,
            move |event| {
                // info!("handle event");
                if let Ok(event) = event {
                    match event.event_type {
                        WebSocketEventType::BeforeConnect => {
                            // info!("Websocket before connect");
                        }
                        WebSocketEventType::Connected => {
                            info!("Websocket connected");
                            // external_sender.send(AppEvent::WebSocketConnected).unwrap();
                            match inner_sender.send(AppEvent::WebSocketConnected) {
                                Ok(_) => {}
                                Err(e) => {
                                    error!("Error sending audio data: {:?}", e);
                                }
                            }
                        }
                        WebSocketEventType::Disconnected => {
                            // 旧连接销毁时迟到的断开事件直接丢弃，防止误伤新会话
                            if conn_epoch.load(Ordering::SeqCst) != my_epoch {
                                info!("Ignore stale websocket Disconnected event");
                                return;
                            }
                            // 通知 open_audio_channel 的等待循环立即感知失败
                            // （如服务器未启动），否则它会永远阻塞在 recv() 上
                            let _ = inner_sender.send(AppEvent::WebSocketClosed);
                            external_sender.send(AppEvent::WebSocketClosed).unwrap();
                        }

                        WebSocketEventType::Close(reason) => {
                            if conn_epoch.load(Ordering::SeqCst) != my_epoch {
                                info!("Ignore stale websocket Close event");
                                return;
                            }
                            info!("Websocket close, reason: {reason:?}");
                            let _ = inner_sender.send(AppEvent::WebSocketClosed);
                            external_sender.send(AppEvent::WebSocketClosed).unwrap();
                        }

                        WebSocketEventType::Closed => {
                            if conn_epoch.load(Ordering::SeqCst) != my_epoch {
                                info!("Ignore stale websocket Closed event");
                                return;
                            }
                            let _ = inner_sender.send(AppEvent::WebSocketClosed);
                            external_sender.send(AppEvent::WebSocketClosed).unwrap();
                            info!("Websocket closed");
                        }

                        WebSocketEventType::Text(text) => {
                            info!("Websocket received a text message, text: {text}");

                            if !*server_hello_received.lock().unwrap() {
                                // let message: serde_json::Value =
                                //     serde_json::from_str(text).unwrap();

                                match serde_json::from_str::<serde_json::Value>(text) {
                                    Ok(message) => {
                                        if let Some(message_type) = message["type"].as_str() {
                                            if message_type == "hello" {
                                                inner_sender
                                                    .send(AppEvent::ServerHelloMessageReceived(
                                                        text.to_string(),
                                                    ))
                                                    .unwrap();
                                                *server_hello_received.lock().unwrap() = true;
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        if text == "认证失败" {
                                            //TODO:: 处理认证失败！
                                        } else {
                                            error!("received a unknown message: {:?}", err);
                                        }
                                    }
                                }
                            } else {
                                external_sender
                                    .send(AppEvent::WebsocketTextMessageReceived(text.to_string()))
                                    .unwrap();
                            }

                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                        }

                        WebSocketEventType::Binary(binary) => {
                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                            // info!("Websocket recv, binary len: {}", binary.len());
                            let packet = AudioStreamPacket {
                                sample_rate: 16000,
                                frame_duration: 60,
                                timestamp: 0,
                                payload: binary.to_vec(),
                            };

                            external_sender
                                .send(AppEvent::AudioPacketReceived(packet))
                                .unwrap();
                        }
                        WebSocketEventType::Ping => {
                            // 心跳也算"连接活跃"：否则空闲 120 秒后（服务器只发 ping
                            // 不发业务数据时）is_timeout() 误判超时，下次唤醒会白白
                            // 走一遍 close + 重连流程
                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                            info!("Websocket ping");
                        }
                        WebSocketEventType::Pong => {
                            *last_incoming_time.lock().unwrap() = Some(Instant::now());
                            info!("Websocket pong");
                        }
                    }
                }
            },
        )?));

        // // info!("wait for server hello message");
        // // wait for server hello message
        // let receiver = inner_receiver;

        // if let Some(rx) = receiver {
        // 等待 server hello 的总超时：连接超时(timeout) + 重连/握手余量。
        // 没有 || 等待逻辑时（服务器未启动、网络不通、服务器不回 hello），
        // recv() 会永久阻塞，主事件循环卡死在 Connecting 状态
        let hello_deadline = Instant::now() + timeout + Duration::from_secs(5);
        loop {
            info!("WebSocketProtocol: Waiting for server hello message...");
            let remaining = hello_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                error!("Timeout waiting for server hello message");
                self.close_audio_channel()?;
                return Err(Error::msg("timeout waiting for server hello"));
            }
            match inner_receiver.recv_timeout(remaining) {
                Ok(event) => {
                    match event {
                        AppEvent::WebSocketConnected => {
                            info!("WebSocketConnected,try to send hello message");
                            // send client hello message
                            if let Some(client) = &mut self.client {
                                let hello_message = ClientHelloMessage::new().unwrap();
                                debug!("WebSocketProtocol: Sending hello message...");
                                match client.send(FrameType::Text(false), hello_message.as_bytes())
                                {
                                    Ok(_) => {
                                        debug!("WebSocketProtocol: Hello message sent!")
                                    }
                                    Err(e) => {
                                        error!("WebSocketProtocol: Send error: {:?}", e)
                                    }
                                }
                            }
                        }
                        AppEvent::ServerHelloMessageReceived(_) => {
                            self.is_connected = true;
                            break;
                        }
                        // 连接失败（如服务器未启动）：后台 auto-reconnect 还会重试，
                        // 但这里立即放弃并清理，让调用方恢复 Idle，下次唤醒再试
                        AppEvent::WebSocketClosed => {
                            error!("WebSocket disconnected before server hello");
                            self.close_audio_channel()?;
                            return Err(Error::msg(
                                "websocket disconnected before server hello",
                            ));
                        }
                        _ => {}
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    error!("Timeout waiting for server hello message");
                    self.close_audio_channel()?;
                    return Err(Error::msg("timeout waiting for server hello"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // inner channel 意外关闭（闭包/client 已被销毁）
                    error!("Inner websocket event channel closed unexpectedly");
                    self.close_audio_channel()?;
                    return Err(Error::msg("inner channel closed before server hello"));
                }
            }
        }
        // } else {
        //     error!("websocket error: internal_receiver is None");
        // }

        info!("end of open_audio_channel.");
        self.is_connected = true;

        Ok(true)
    }

    fn close_audio_channel(&mut self) -> Result<(), Error> {
        // 递增连接世代：使旧连接销毁过程中迟到的断开事件全部失效
        self.conn_epoch.fetch_add(1, Ordering::SeqCst);
        // 无条件销毁 client：不仅已连接的要关，连接中途/失败的也要清理。
        // esp-idf-svc 的 websocket 默认开启 auto-reconnect，失败后会在后台
        // 无限重试并不断往事件队列灌 Disconnected，必须 take 掉才会停止。
        // （take 获取所有权，本作用域结束时自动销毁 websocket 任务）
        let _ = self.client.take();

        self.is_connected = false;
        *self.last_incoming_time.lock().unwrap() = None;

        // self.external_sender.send(XzEvent::WebSocketClosed).unwrap();
        Ok(())
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
        Ok(())
    }

    fn send_wake_word_detected(&mut self, wake_word: &str) -> Result<(), Error> {
        // 对应 C++ ProtocolWebsocket::SendWakeWordDetected
        // {"session_id":"...","type":"listen","state":"detect","text":"你好小智"}
        let message = format!(
            r##"{{"session_id": "{}",
    "type": "listen",
    "state": "detect",
    "text": "{}"}}"##,
            self.device_id, wake_word
        );
        self.send_text(&message)?;
        Ok(())
    }

    fn send_start_linstening(&mut self, listening_mode: ListeningMode) -> Result<(), Error> {
        // std::string message = "{\"session_id\":\"" + session_id_ + "\"";
        // message += ",\"type\":\"listen\",\"state\":\"start\"";
        // if (mode == kListeningModeRealtime) {
        //     message += ",\"mode\":\"realtime\"";
        // } else if (mode == kListeningModeAutoStop) {
        //     message += ",\"mode\":\"auto\"";
        // } else {
        //     message += ",\"mode\":\"manual\"";
        // }
        // message += "}";
        // SendText(message);

        let mode = match listening_mode {
            ListeningMode::AutoStop => "auto",
            ListeningMode::Realtime => "realtime",
            ListeningMode::Manual => "manual",
        };

        let message = format!(
            r##"{{"session_id": "{}",
    "type": "listen",
    "state": "start",
    "mode": "{}"}}"##,
            self.device_id, mode
        );
        self.send_text(&message)?;
        Ok(())
    }

    fn send_stop_listening(&mut self) -> Result<(), Error> {
        //     std::string message = "{\"session_id\":\"" + session_id_ + "\",\"type\":\"listen\",\"state\":\"stop\"}";
        // SendText(message);
        let message = format!(
            r##"{{"session_id": "{}",
    "type": "listen",
    "state": "stop"}}"##,
            self.device_id
        );
        self.send_text(&message)?;
        Ok(())
    }

    fn on_network_error<F>(&mut self, handler: F)
    where
        F: FnMut(&str) -> Result<()> + Send + 'static,
    {
        self.on_network_error = Some(Box::new(handler));
    }

    fn set_connected(&mut self, connected: bool) {
        info!("WebSocket connection status changed: {}", connected);
        self.is_connected = connected;
    }
}

impl Drop for WebSocketProtocol {
    fn drop(&mut self) {
        if let Err(err) = self.close_audio_channel() {
            error!("WebSocketProtocol: Close audio channel error: {:?}", err);
        }
    }
}

#[derive(Clone, Debug)]
pub enum WebSocketHelloEvent {
    ServerHelloEvent,
}
