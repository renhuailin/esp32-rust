pub trait AudioProcessor: Send {
    fn initialize(&mut self);
    fn feed(&mut self, data: &[i16]);
    fn start(&mut self);
    fn stop(&mut self);
    fn is_running(&self) -> bool;

    // 下面的写法是非 dyn compatible 的写法。
    // fn on_output(&mut self, callback: impl FnMut(Vec<i16>) + Send + 'static);
    // fn on_vad_state_change(&mut self, callback: impl FnMut(bool) + Send + 'static);

    fn on_output(&mut self, callback: Box<dyn FnMut(Vec<i16>) + Send + 'static>);
    fn on_vad_state_change(&mut self, callback: Box<dyn FnMut(bool) + Send + 'static>);

    fn get_feed_size(&self) -> usize;
    fn enable_device_aec(&mut self, enable: bool);
}
