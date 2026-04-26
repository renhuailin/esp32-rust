#[derive(Copy, Clone, Debug, PartialEq)]
pub enum AbortReason {
    None,
    WakeWordDetected,
}

#[derive(PartialEq, Clone, Debug)]
pub enum ListeningMode {
    AutoStop,
    Realtime,
    Manual,
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

#[derive(PartialEq, Debug, Clone)]
pub enum I2SFormat {
    Min = -1,
    Normal = 0,
    Left = 1,
    Right = 2,
    DSP = 3,
    Max,
}
