use anyhow::Result;
use esp_idf_hal::gpio::InputPin;
use esp_idf_hal::gpio::OutputPin;
use esp_idf_hal::i2s::config;
use esp_idf_hal::i2s::I2s;
use esp_idf_hal::i2s::I2sDriver;
use esp_idf_hal::sys;
use esp_idf_sys::esp;
use esp_idf_sys::EspError;
use log::info;
use std::ptr;
use thiserror::Error;
pub fn create_std_tx_tdm_rx() -> Result<(), Error> {
    // ===============================================================
    // == 核心：手动创建和配置混合模式I2S通道
    // ===============================================================

    let mut tx_handle: sys::i2s_chan_handle_t = ptr::null_mut();
    let mut rx_handle: sys::i2s_chan_handle_t = ptr::null_mut();

    // --- 1. 创建一个I2S通道对 (TX 和 RX) ---
    info!("Creating I2S channel pair...");
    let chan_cfg = sys::i2s_chan_config_t {
        id: sys::i2s_port_t_I2S_NUM_0,
        role: sys::i2s_role_t_I2S_ROLE_MASTER,
        dma_desc_num: 8,
        dma_frame_num: 240,
        ..Default::default()
    };
    // 调用底层的C API来创建通道
    sys::EspError::from(unsafe {
        sys::i2s_new_channel(&chan_cfg, &mut tx_handle, &mut rx_handle)
    })?;
    info!("I2S channel pair created successfully.");

    // --- 2. 配置并初始化发送(TX)通道为标准(STD)模式 ---
    info!("Initializing TX channel in Standard (Philips) mode...");
    let std_cfg = sys::i2s_std_config_t {
        clk_cfg: sys::i2s_std_clk_config_t {
            sample_rate_hz: 16000, // 您的播放采样率
            clk_src: sys::i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
            mclk_multiple: sys::i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
            ..Default::default()
        },
        slot_cfg: sys::i2s_std_slot_config_t {
            data_bit_width: sys::i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
            slot_bit_width: sys::i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO,
            slot_mode: sys::i2s_slot_mode_t_I2S_SLOT_MODE_STEREO,
            slot_mask: sys::i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH,
            ws_width: sys::i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
            bit_shift: true,
            left_align: false, // 注意：C++代码中这里是true，但对于Philips模式，false更标准
            ..Default::default()
        },
        gpio_cfg: sys::i2s_std_gpio_config_t {
            mclk: pins.gpio41.pin(),
            bclk: pins.gpio42.pin(),
            ws: pins.gpio40.pin(),
            dout: pins.gpio39.pin(),
            din: sys::gpio_num_t_GPIO_NUM_NC, // TX通道不使用DIN
            ..Default::default()
        },
    };
    sys::EspError::from(unsafe { sys::i2s_channel_init_std_mode(tx_handle, &std_cfg) })?;
    info!("TX channel initialized.");

    // --- 3. 配置并初始化接收(RX)通道为TDM模式 ---
    info!("Initializing RX channel in TDM mode...");
    let tdm_cfg = sys::i2s_tdm_config_t {
        clk_cfg: sys::i2s_tdm_clk_config_t {
            sample_rate_hz: 16000, // 您的录音采样率
            clk_src: sys::i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
            mclk_multiple: sys::i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
            bclk_div: 8,
            ..Default::default()
        },
        slot_cfg: sys::i2s_tdm_slot_config_t {
            data_bit_width: sys::i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
            slot_bit_width: sys::i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO,
            slot_mode: sys::i2s_slot_mode_t_I2S_SLOT_MODE_STEREO,
            slot_mask: sys::i2s_tdm_slot_mask_t_I2S_TDM_SLOT_ALL,
            ws_width: 16, // I2S_TDM_AUTO_WS_WIDTH
            bit_shift: true,
            ..Default::default()
        },
        gpio_cfg: sys::i2s_gpio_config_t {
            mclk: sys::gpio_num_t_GPIO_NUM_NC, // RX通道可以不重复指定MCLK
            bclk: sys::gpio_num_t_GPIO_NUM_NC,
            ws: sys::gpio_num_t_GPIO_NUM_NC,
            dout: sys::gpio_num_t_GPIO_NUM_NC, // RX通道不使用DOUT
            din: pins.gpio45.pin(),
            ..Default::default()
        },
    };
    sys::EspError::from(unsafe { sys::i2s_channel_init_tdm_mode(rx_handle, &tdm_cfg) })?;
    info!("RX channel initialized.");

    // --- 4. 启用通道 ---
    info!("Enabling I2S channels...");
    unsafe {
        sys::i2s_channel_enable(tx_handle);
        sys::i2s_channel_enable(rx_handle);
    }
    info!("I2S channels are now active.");
}
