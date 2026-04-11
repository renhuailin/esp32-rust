use std::{ffi::c_void, ptr};

use esp_idf_sys::vTaskDelete;

/// C 任务跳板函数，用于执行 Rust 闭包
type TaskClosure = Box<dyn FnOnce() + Send + 'static>;

pub unsafe extern "C" fn c_task_trampoline(arg: *mut c_void) {
    // 这里 arg 就是你当初传入的那个 TaskClosure 指针
    // Box::from_raw 会安全地从这个指针重建 Box
    let closure = Box::from_raw(arg as *mut TaskClosure);

    // 执行闭包
    closure();

    // 必须删除任务
    vTaskDelete(ptr::null_mut());
}
