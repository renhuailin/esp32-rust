/// Represents the gain settings for the ES7210 microphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)] // 告诉编译器用一个u8来表示这个enum
pub enum MicGain {
    Gain0db = 0,
    Gain3db,
    Gain6db,
    Gain9db,
    Gain12db,
    Gain15db,
    Gain18db,
    Gain21db,
    Gain24db,
    Gain27db,
    Gain30db,
    Gain33db,
    Gain34_5db,
    Gain36db,
    Gain37_5db,
}

impl MicGain {
    /// 获取该增益设置对应的、需要写入寄存器的整数值。
    pub fn value(&self) -> u8 {
        // `*self as u8` 可以安全地将enum变体转换为其对应的整数值
        *self as u8
    }
}
