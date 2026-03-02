use esp_idf_sys::mbedtls_md5;

pub fn calc_md5_builtin(data: &[u8]) -> String {
    // MD5 的结果固定为 16 字节
    let mut output = [0u8; 16];

    unsafe {
        let ret = mbedtls_md5(data.as_ptr(), data.len(), output.as_mut_ptr());

        if ret != 0 {
            log::error!("mbedtls_md5 failed with error code: {}", ret);
            return String::new();
        }
    }

    // 将 16 字节格式化为 32 个字符的十六进制字符串
    output
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}
