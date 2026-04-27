use anyhow::{anyhow, Result};
use esp_idf_sys::*;
use log::info;
use std::ptr;

// 这些常量如果不在你的 bindgen 输出中，可能需要你手动确认，通常是 6 和 240
const AUDIO_CODEC_DMA_DESC_NUM: u32 = 6;
const AUDIO_CODEC_DMA_FRAME_NUM: u32 = 240;

pub struct MixedI2sDriver {
    tx_handle: i2s_chan_handle_t,
    rx_handle: i2s_chan_handle_t,
}

// 必须实现 Send/Sync 才能在多线程中使用
unsafe impl Send for MixedI2sDriver {}
unsafe impl Sync for MixedI2sDriver {}

impl MixedI2sDriver {
    pub fn new(
        sample_rate: u32,
        mclk: i32,
        bclk: i32,
        ws: i32,
        dout: i32,
        din: i32,
    ) -> Result<Self> {
        let mut tx_handle: i2s_chan_handle_t = ptr::null_mut();
        let mut rx_handle: i2s_chan_handle_t = ptr::null_mut();

        unsafe {
            // 1. 初始化通用通道配置
            let mut chan_cfg: i2s_chan_config_t = std::mem::zeroed();
            chan_cfg.id = i2s_port_t_I2S_NUM_0;
            chan_cfg.role = i2s_role_t_I2S_ROLE_MASTER;
            chan_cfg.dma_desc_num = AUDIO_CODEC_DMA_DESC_NUM;
            chan_cfg.dma_frame_num = AUDIO_CODEC_DMA_FRAME_NUM;
            chan_cfg.auto_clear_before_cb = true;
            chan_cfg.intr_priority = 0;

            let ret = i2s_new_channel(&chan_cfg, &mut tx_handle, &mut rx_handle);
            if ret != ESP_OK {
                return Err(anyhow!("Failed to create I2S channels, err: {}", ret));
            }

            // 2. 配置 TX (Std Mode - 用于喇叭播放)
            let mut std_cfg: i2s_std_config_t = std::mem::zeroed();

            // 2.1 时钟配置
            std_cfg.clk_cfg.sample_rate_hz = sample_rate;
            std_cfg.clk_cfg.clk_src = soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT;
            std_cfg.clk_cfg.mclk_multiple = i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256;

            // 2.2 Slot 配置
            std_cfg.slot_cfg.data_bit_width = i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT;
            std_cfg.slot_cfg.slot_bit_width = i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO;
            std_cfg.slot_cfg.slot_mode = i2s_slot_mode_t_I2S_SLOT_MODE_STEREO;
            std_cfg.slot_cfg.slot_mask = i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH;
            std_cfg.slot_cfg.ws_width = i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT;
            std_cfg.slot_cfg.ws_pol = false;
            std_cfg.slot_cfg.bit_shift = true;
            std_cfg.slot_cfg.left_align = true;
            std_cfg.slot_cfg.big_endian = false;
            std_cfg.slot_cfg.bit_order_lsb = false;

            // 2.3 GPIO 配置 (TX 只用 dout)
            std_cfg.gpio_cfg.mclk = mclk;
            std_cfg.gpio_cfg.bclk = bclk;
            std_cfg.gpio_cfg.ws = ws;
            std_cfg.gpio_cfg.dout = dout;
            std_cfg.gpio_cfg.din = -1; // -1 相当于 I2S_GPIO_UNUSED

            let ret = i2s_channel_init_std_mode(tx_handle, &std_cfg);
            if ret != ESP_OK {
                return Err(anyhow!("Failed to init TX in Std mode, err: {}", ret));
            }

            // 3. 配置 RX (TDM Mode - 用于四麦克风录音)
            let mut tdm_cfg: i2s_tdm_config_t = std::mem::zeroed();

            // 3.1 时钟配置
            tdm_cfg.clk_cfg.sample_rate_hz = sample_rate;
            tdm_cfg.clk_cfg.clk_src = soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT;
            tdm_cfg.clk_cfg.mclk_multiple = i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256;
            tdm_cfg.clk_cfg.bclk_div = 8; // 注意 C++ 中这一项是有的

            // 3.2 Slot 配置 (开启 4 个 Slot)
            tdm_cfg.slot_cfg.data_bit_width = i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT;
            tdm_cfg.slot_cfg.slot_bit_width = i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO;
            tdm_cfg.slot_cfg.slot_mode = i2s_slot_mode_t_I2S_SLOT_MODE_STEREO;
            tdm_cfg.slot_cfg.slot_mask = i2s_tdm_slot_mask_t_I2S_TDM_SLOT0
                | i2s_tdm_slot_mask_t_I2S_TDM_SLOT1
                | i2s_tdm_slot_mask_t_I2S_TDM_SLOT2
                | i2s_tdm_slot_mask_t_I2S_TDM_SLOT3;

            tdm_cfg.slot_cfg.ws_width = I2S_TDM_AUTO_WS_WIDTH;
            tdm_cfg.slot_cfg.ws_pol = false;
            tdm_cfg.slot_cfg.bit_shift = true;
            tdm_cfg.slot_cfg.left_align = false; // 注意 C++ 这里是 false
            tdm_cfg.slot_cfg.big_endian = false;
            tdm_cfg.slot_cfg.bit_order_lsb = false;
            tdm_cfg.slot_cfg.skip_mask = false;
            tdm_cfg.slot_cfg.total_slot = I2S_TDM_AUTO_SLOT_NUM;

            // 3.3 GPIO 配置 (RX 只用 din)
            tdm_cfg.gpio_cfg.mclk = mclk;
            tdm_cfg.gpio_cfg.bclk = bclk;
            tdm_cfg.gpio_cfg.ws = ws;
            tdm_cfg.gpio_cfg.dout = -1; // I2S_GPIO_UNUSED
            tdm_cfg.gpio_cfg.din = din;

            let ret = i2s_channel_init_tdm_mode(rx_handle, &tdm_cfg);
            if ret != ESP_OK {
                return Err(anyhow!("Failed to init RX in TDM mode, err: {}", ret));
            }

            info!("Duplex channels created successfully (TX: Std, RX: TDM)");

            // // 4. 必须手动启用两个通道
            // i2s_channel_enable(tx_handle);
            // i2s_channel_enable(rx_handle);

            Ok(Self {
                tx_handle,
                rx_handle,
            })
        }
    }

    pub fn rx_enable(&mut self) -> Result<(), EspError> {
        unsafe {
            info!("I2S RX channel enabled");
            esp!(i2s_channel_enable(self.rx_handle))
            // i2s_channel_enable(self.tx_handle);
        }
    }

    pub fn tx_enable(&mut self) -> Result<(), EspError> {
        info!("I2S TX channel enabled");
        unsafe { esp!(i2s_channel_enable(self.tx_handle)) }
    }

    /// 从 I2S 读取数据 (TDM 格式，包含 4 个通道数据)
    pub fn read(&mut self, dest: &mut [u8], timeout_ms: u32) -> Result<usize> {
        let mut bytes_read: usize = 0;
        unsafe {
            // 参数 1000 是超时 ticks (portMAX_DELAY 也可以)
            let ret = i2s_channel_read(
                self.rx_handle,
                dest.as_mut_ptr() as *mut _,
                dest.len(),
                &mut bytes_read,
                timeout_ms,
            );
            if ret != ESP_OK {
                return Err(anyhow!("I2S read error: {}", ret));
            }
        }
        Ok(bytes_read)
    }

    /// 写入数据到 I2S (Std 格式，用于播放)
    pub fn write(&mut self, src: &[u8], timeout_ms: u32) -> Result<usize> {
        let mut bytes_written: usize = 0;
        unsafe {
            let ret = i2s_channel_write(
                self.tx_handle,
                src.as_ptr() as *const _,
                src.len(),
                &mut bytes_written,
                timeout_ms,
            );
            if ret != ESP_OK {
                return Err(anyhow!("I2S write error: {}", ret));
            }
        }
        Ok(bytes_written)
    }
}

// 确保在结构体 Drop 时禁用和删除通道，防止内存/硬件资源泄漏
impl Drop for MixedI2sDriver {
    fn drop(&mut self) {
        unsafe {
            if !self.tx_handle.is_null() {
                i2s_channel_disable(self.tx_handle);
                i2s_del_channel(self.tx_handle);
            }
            if !self.rx_handle.is_null() {
                i2s_channel_disable(self.rx_handle);
                i2s_del_channel(self.rx_handle);
            }
        }
    }
}
