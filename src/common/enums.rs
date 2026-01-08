#[derive(Copy, Clone, Debug, PartialEq)]
pub enum AbortReason {
    None,
    WakeWordDetected,
}

#[derive(PartialEq)]
pub enum ListeningMode {
    AutoStop,
    Realtime,
}

#[derive(PartialEq)]
pub enum AecMode {
    Off,
    On,
}

#[derive(PartialEq)]
pub enum DeviceState {
    Idle,
    Activating,
    WifiConfiguring,
    Connecting,
    DeviceStateAudioTesting,
    Speaking,
    Listening,
    Starting,
}
