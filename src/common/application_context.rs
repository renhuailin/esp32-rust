use std::sync::mpsc::Sender;

use crate::common::event::AppEvent;

pub struct ApplicationContext {
    pub app_event_sender: Sender<AppEvent>,
}
