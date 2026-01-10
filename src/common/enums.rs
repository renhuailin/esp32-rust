#[derive(Copy, Clone, Debug, PartialEq)]
pub enum AbortReason {
    None,
    WakeWordDetected,
}

#[derive(PartialEq, Clone, Debug)]
pub enum ListeningMode {
    AutoStop,
    Realtime,
}

#[derive(PartialEq, Clone, Debug)]
pub enum AecMode {
    Off,
    On,
}

#[derive(PartialEq, Debug, Clone)]
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
