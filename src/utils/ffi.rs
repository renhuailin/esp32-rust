use std::{ffi::c_void, ptr};

/// C 任务跳板函数，用于执行 Rust 闭包
pub unsafe extern "C" fn c_task_trampoline(arg: *mut c_void) {
    // 1. 将 void* 转回 Box<dyn FnOnce()>
    // 注意：这里的类型必须与你 into_raw 传入的一致
    let rust_closure = Box::from_raw(arg as *mut Box<dyn FnOnce()>);

    // 2. 执行闭包
    rust_closure();

    // 3. 任务结束，手动删除 (在 ESP32 中，任务函数不能简单退出，必须删除)
    esp_idf_sys::vTaskDelete(ptr::null_mut());
}
