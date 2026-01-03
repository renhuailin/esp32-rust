use anyhow::Result;
use esp_idf_hal::gpio::AnyIOPin;
use esp_idf_hal::task::asynch::Notification;
use esp_idf_svc::hal::gpio::{InterruptType, PinDriver, Pull};
use std::{num::NonZeroU32, sync::Arc};

// 这是一个可复用的Button结构体
pub struct Button<'d> {
    // 我们不再需要PinDriver的可变访问，所以可以设为私有
    _pin: PinDriver<'d, AnyIOPin, esp_idf_svc::hal::gpio::Input>,
    notification: Arc<Notification>,
}

impl<'d> Button<'d> {
    /// 创建并配置一个新的按钮实例
    pub fn new(pin: impl Into<AnyIOPin> + 'd) -> Result<Self> {
        // 1. 配置引脚
        let mut button_pin = PinDriver::input(pin.into())?;
        button_pin.set_pull(Pull::Up)?;
        button_pin.set_interrupt_type(InterruptType::PosEdge)?;

        // 2. 每个按钮都有自己的“信号旗”
        let notification = Arc::new(Notification::new());
        let notifier = Arc::clone(&notification);
        // let notifier = notification.notifier();

        // 3. 订阅中断
        // Safety: `notification`被移动到`Self`中，与`_pin`有相同的生命周期，
        // 只要Button实例存在，notification就存在，所以是安全的。
        unsafe {
            button_pin.subscribe(move || {
                // println!("Button pressed!");
                notifier.notify(NonZeroU32::new(1).unwrap());
            })?;
        }

        // 4. 首次使能中断
        button_pin.enable_interrupt()?;

        Ok(Self {
            _pin: button_pin,
            notification,
        })
    }

    pub fn on_click<F>(&mut self, click_handler: F) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        unsafe {
            self._pin.subscribe(click_handler)?;
        }
        Ok(())
    }

    pub fn enable_interrupt(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        self._pin.enable_interrupt()?;
        Ok(())
    }
    /// 阻塞地等待按钮被按下
    pub async fn wait(&self) {
        // 等待这个按钮专属的信号旗
        self.notification.wait().await;
    }
}
