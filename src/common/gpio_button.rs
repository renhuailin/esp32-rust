use anyhow::Result;
use esp_idf_sys::es32_component_button::{
    button_config_t, button_event_t_BUTTON_SINGLE_CLICK, button_gpio_config_t, button_handle_t,
    iot_button_delete, iot_button_new_gpio_device, iot_button_register_cb,
    iot_button_unregister_cb,
};
use std::ffi::c_void;
use std::ptr;

// 定义一个类型别名，方便阅读：这是一个装箱的、线程安全的、可变的闭包
type BoxedCallback = Box<dyn FnMut() + Send + 'static>;

pub struct Button {
    button_handle: button_handle_t,
    // 我们需要保存回调的指针，原因有两个：
    // 1. 保证闭包在 C 回调期间活着
    // 2. 在 Button Drop 时，我们需要手动释放这块内存，否则会内存泄漏
    // 这里保存的是指向 Box<BoxedCallback> 的裸指针
    callback_ptr: Option<*mut BoxedCallback>,
}

impl Button {
    /// 创建并配置一个新的按钮实例
    pub fn new(gpio_num: i32) -> Result<Self> {
        // 使用 Default 或 zeroed 初始化 C 结构体通常更安全，防止未来字段变动
        let button_config = button_config_t {
            long_press_time: 1000,
            short_press_time: 50,
            ..Default::default()
        };

        let button_gpio_config = button_gpio_config_t {
            gpio_num,
            active_level: 0,
            enable_power_save: false,
            disable_pull: false,
        };

        let mut button_handle: button_handle_t = ptr::null_mut();
        let ret = unsafe {
            iot_button_new_gpio_device(&button_config, &button_gpio_config, &mut button_handle)
        };

        if ret != 0 || button_handle.is_null() {
            return Err(anyhow::anyhow!("Failed to create button device: {}", ret));
        }

        Ok(Self {
            button_handle,
            callback_ptr: None,
        })
    }

    /// 注册单击事件
    pub fn on_click<F>(&mut self, callback: F) -> Result<()>
    where
        F: FnMut() + Send + 'static,
    {
        // 1. 清理旧的回调（如果有）
        self.free_callback();

        // 2. 处理闭包的指针转换
        // 第一步：把闭包 Box 起来，变成 Trait Object (这是一个胖指针)
        let cb_box: BoxedCallback = Box::new(callback);

        // 第二步：因为 Trait Object 是胖指针(2个字长)，不能直接转成 void*(1个字长)。
        // 所以我们需要再套一层 Box，得到指向 Trait Object 的指针 (这是一个瘦指针)
        let cb_wrapper = Box::new(cb_box);

        // 第三步：转为裸指针，准备传给 C
        let usr_data = Box::into_raw(cb_wrapper);

        // 3. 注册回调
        let ret = unsafe {
            iot_button_register_cb(
                self.button_handle,
                button_event_t_BUTTON_SINGLE_CLICK,
                ptr::null_mut(),
                Some(trampoline),        // 使用下面的蹦床函数
                usr_data as *mut c_void, // 传入我们的闭包指针
            )
        };

        if ret != 0 {
            // 如果注册失败，别忘了把内存释放回来
            unsafe {
                let _ = Box::from_raw(usr_data);
            }
            return Err(anyhow::anyhow!("Failed to register callback: {}", ret));
        }

        // 4. 保存指针以便后续释放
        self.callback_ptr = Some(usr_data);

        Ok(())
    }

    // 辅助函数：释放回调占用的内存
    fn free_callback(&mut self) {
        if let Some(ptr) = self.callback_ptr.take() {
            unsafe {
                // 先取消注册 (虽然 iot_button_delete 会处理，但显式处理是个好习惯)
                iot_button_unregister_cb(
                    self.button_handle,
                    button_event_t_BUTTON_SINGLE_CLICK,
                    ptr::null_mut(),
                );
                // 将裸指针转回 Box，让它离开作用域自动 Drop
                let _ = Box::from_raw(ptr);
            }
        }
    }
}

// --- 蹦床函数 (Trampoline) ---
// 这是一个符合 C ABI 的静态函数。
// 它的作用是接收 C 的调用，把 void* 转换回 Rust 的闭包，然后执行。
unsafe extern "C" fn trampoline(_arg: *mut c_void, usr_data: *mut c_void) {
    if usr_data.is_null() {
        return;
    }

    // 1. 将 void* 转换回 Box<BoxedCallback> 的指针
    let callback_ptr = usr_data as *mut BoxedCallback;

    // 2. 获取闭包的可变引用
    let callback = &mut *callback_ptr;

    // 3. 调用闭包
    // 这里的 panic 可能会导致 C 栈展开问题，生产环境建议 catch_unwind
    (callback)();
}

impl Drop for Button {
    fn drop(&mut self) {
        // 1. 先释放回调的内存
        self.free_callback();

        // 2. 再删除按钮句柄
        if !self.button_handle.is_null() {
            unsafe {
                iot_button_delete(self.button_handle);
            }
        }
    }
}
