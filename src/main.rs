use std::collections::VecDeque;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, MutexGuard};
use std::{thread, time::Duration};

use anyhow::{anyhow, Error, Result};
use crossbeam_channel::unbounded;
use esp_idf_hal::delay;
use esp_idf_hal::i2s::config::{
    ClockSource, MclkMultiple, TdmClkConfig, TdmConfig, TdmGpioConfig, TdmSlot, TdmSlotConfig,
    TdmSlotMask,
};
use esp_idf_hal::i2s::I2sTx;
use esp_idf_hal::task::asynch::Notification;
use esp_idf_hal::task::block_on;
use esp_idf_hal::{
    delay::{Delay, FreeRtos, BLOCK},
    gpio::{self, AnyIOPin, AnyInputPin, AnyOutputPin, PinDriver},
    i2c::{I2cConfig, I2cDriver},
    i2s::{
        config::{Config, DataBitWidth, Role, StdConfig},
        I2sBiDir, I2sDriver,
    },
    ledc::{config::TimerConfig, LedcDriver, LedcTimerDriver},
    prelude::*,
    rmt::RmtChannel,
    spi::SpiDriver,
};
use esp_idf_svc::hal::prelude::*;
use esp_idf_svc::{eventloop::EspSystemEventLoop, timer::EspTaskTimerService};
use esp_idf_sys::EspError;
use futures::{select, FutureExt};
use log::{error, info};
use mipidsi::error;
use shared_bus::BusManagerSimple;
use xiaoxin_esp32::audio::es7210::es7210::Es7210;
use xiaoxin_esp32::audio::opus::decoder::OpusAudioDecoder;
use xiaoxin_esp32::audio::{AUDIO_INPUT_SAMPLE_RATE, I2S_MCLK_MULTIPLE_256};
use xiaoxin_esp32::common::button;
use xiaoxin_esp32::{
    audio,
    axp173::{Axp173, Ldo},
    lcd,
    led::WS2812RMT,
    wifi::wifi,
};
use xiaoxin_esp32::{wifi, Application, ApplicationState};
// 1. 引入 std::sync::mpsc
use std::sync::mpsc::{channel, Receiver, Sender};

// 使用VecDeque作为缓冲区，因为它在头部移除元素时效率很高
pub type AudioBuffer = VecDeque<u8>;

// 共享状态结构体
pub struct SharedAudioState {
    pub buffer: Mutex<AudioBuffer>,
    // 我们可以添加一个Condvar，以便在录音满或播放空时进行等待
    // 但为了简单起见，我们先只用Mutex
}

impl SharedAudioState {
    pub fn new() -> Self {
        Self {
            buffer: Mutex::new(VecDeque::new()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum AudioCommand {
    StartRecording,
    StopAndPlayback,
}

fn main() -> Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();
    let peripherals: Peripherals = Peripherals::take().unwrap();

    // let app_config = CONFIG;
    let pins = peripherals.pins;
    let spi3 = peripherals.spi3;

    // 1. 初始化I2C总线。
    //    !!! 警告: 您必须根据开发板的原理图，确认AXP173连接的是哪个I2C总线和引脚！
    let sda = pins.gpio1;
    let scl = pins.gpio2;
    let i2c = peripherals.i2c1;
    let config = I2cConfig::new();

    let i2c_driver = I2cDriver::new(i2c, sda, scl, &config).unwrap();

    // 2. 创建一个总线管理器，并将I2C驱动的所有权交给它
    let bus_manager = BusManagerSimple::new(i2c_driver);

    // 3. 从管理器中为每个设备创建独立的“代理”
    //    axp_i2c_proxy 和 es_i2c_proxy 现在是两个可以独立使用的I2C设备
    let axp173_i2c_proxy = bus_manager.acquire_i2c();
    let es8311_i2c_proxy = bus_manager.acquire_i2c();
    let es7210_i2c_proxy = bus_manager.acquire_i2c();

    let sysloop = EspSystemEventLoop::take()?;
    let wifi = wifi::wifi(
        "CU_liu81802",
        "china-ops",
        peripherals.modem,
        sysloop.clone(),
    )?;

    {
        // 2. 创建AXP173驱动实例
        let mut axp173 = Axp173::new(axp173_i2c_proxy);
        axp173.init().unwrap();

        // 根据axp173手册，LDO4的电压由一个byte,8位bit表示，电压范围是：0.7-3.5V， 25mV/step，每个bit表示25mV。
        // 所以要设置LDO4的电压为3.3V  (3300 - 700) / 25 = 104
        let ldo4 = Ldo::ldo4_with_voltage(104, true);

        // 根据axp173手册，LDO2,LDO3的电压由一个byte,低4位bit表示LDO3的电压，高4位表示LDO2的电压，电压范围是：1.8-3.3V， 100mV/step，每个bit表示100mV。
        // 所以要设置LDO2,LDO3的电压为2.8V  (2800 - 1800) / 100 = 10
        let ldo2 = Ldo::ldo2_with_voltage(10, true);
        axp173.enable_ldo(&ldo2).unwrap();
        axp173.enable_ldo(&ldo4).unwrap();

        let power_control_value = axp173.read_u8(0x33).unwrap();
        println!("Power controller reg33: {:08b}", power_control_value);

        let reg12_value = axp173.read_u8(0x12).unwrap();
        println!("Power controller reg12: {:08b}", reg12_value);

        axp173.set_exten(true).unwrap();
    }

    // 初始化 LCD 屏幕

    // 1. 配置LEDC定时器
    let timer_driver = LedcTimerDriver::new(
        peripherals.ledc.timer0,
        &TimerConfig::new().frequency(25000.Hz().into()),
    )
    .unwrap();

    let backlight_pin = pins.gpio8;

    // 2. 配置LEDC通道，并绑定到背光引脚
    let mut channel_led =
        LedcDriver::new(peripherals.ledc.channel0, timer_driver, backlight_pin).unwrap();

    // 3. 设置亮度 (通过设置占空比)
    let max_duty = channel_led.get_max_duty();
    channel_led.set_duty(max_duty * 3 / 4).unwrap(); // 设置为50%的亮度

    // 初始化 ST7789 屏幕

    // // 2. 根据 diagram.json 配置引脚
    // // 控制引脚
    // // #define DISPLAY_MOSI_PIN      GPIO_NUM_4
    // // #define DISPLAY_CLK_PIN       GPIO_NUM_5
    // // #define DISPLAY_DC_PIN        GPIO_NUM_7
    // // #define DISPLAY_RST_PIN       GPIO_NUM_NC
    // // #define DISPLAY_CS_PIN        GPIO_NUM_6
    let dc = pins.gpio7;
    // SPI 总线引脚 (使用硬件 SPI2)
    let sck = pins.gpio5;
    let sdi = pins.gpio4; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
    let sdo = Option::<AnyInputPin>::None; // MISO
    let cs = pins.gpio6; // 直接使用引脚，而不是PinDriver

    // 3. 初始化 SPI 驱动
    // 创建 SPI 驱动程序实例
    let driver = SpiDriver::new(
        spi3, // 使用 SPI3
        sck,
        sdi,
        sdo,
        &Default::default(),
    )
    .unwrap();

    lcd::LcdSt7789::init(driver, dc.into(), cs.into());

    {
        let audio_decoder = OpusAudioDecoder::new(48000, 1).unwrap();
    }

    //关闭背光

    // // show led demo
    // let led = pins.gpio38;
    // let channel: esp_idf_hal::rmt::CHANNEL0 = peripherals.rmt.channel0;
    // led_demo(led.into(), channel)

    /*
    // 初始化ILI9341
    let dc = pins.gpio4;
    let rst = pins.gpio8; // 即使 diagram.json 没连，驱动也需要这个对象

    // SPI 总线引脚 (使用硬件 SPI2)
    let sck = pins.gpio12;
    let sdi = pins.gpio11; // MOSI 在驱动中通常被称为 SDI (Serial Data In)
    let sdo = pins.gpio13; // MISO
    let cs = pins.gpio10; // 直接使用引脚，而不是PinDriver

    let driver: SpiDriver<'_> = SpiDriver::new(
        peripherals.spi2, // 使用 SPI2
        sck,
        sdi,
        Some(sdo),
        &Default::default(),
    )
    .unwrap();

    lcd::LcdIli9341::init(driver, dc.into(), rst.into(), cs.into());
    */

    //初始化es8311音频解码器
    let mut es8311 = audio::es8311::Es8311::new(es8311_i2c_proxy);

    // match es8311.read_u8(0xFD) {
    //     Ok(chip_id) => {
    //         info!("SUCCESS! Successfully read from ES8311.");
    //         info!("Chip ID: 0x{:02X} (should be 0x83)", chip_id);

    //         info!("Now attempting full init...");
    //     }
    //     Err(_) => {
    //         println!("FATAL: Failed to read from ES8311 at address 0x18.");
    //         println!("Please check: ");
    //         println!("  1. ES8311 Power Supply (is LDO3 correct?).");
    //         println!("  2. I2C Pin connections (GPIO1 for SDA, GPIO2 for SCL?).");
    //         println!("  3. Physical wiring and soldering.");
    //     }
    // }

    Delay::new_default().delay_ms(1000);

    let mut delay = Delay::new_default();

    // es8311.init(&mut delay).unwrap();

    match es8311.open(&mut delay) {
        Ok(_) => {
            println!("初始化ES8311成功");
        }
        Err(e) => {
            println!("初始化ES8311失败:{:?}", e);
            return Err(anyhow!("初始化ES8311失败:{:?}", e));
        }
    }

    // 初始化I2S
    let std_config = StdConfig::philips(16000, DataBitWidth::Bits16);

    // let default_dma_buffer_count = 6;
    // let default_frames_per_dma_buffer = 240;
    // let i2s_channel_config = Config::new()
    //     .dma_buffer_count(default_dma_buffer_count)
    //     .frames_per_buffer(default_frames_per_dma_buffer);
    // let tdm_config = TdmConfig::new(
    //     i2s_channel_config,
    //     TdmClkConfig::new(
    //         AUDIO_INPUT_SAMPLE_RATE,
    //         ClockSource::default(),
    //         MclkMultiple::M256,
    //     ),
    //     TdmSlotConfig::philips_slot_default(
    //         DataBitWidth::Bits16,
    //         TdmSlot::Slot0 | TdmSlot::Slot1 | TdmSlot::Slot2 | TdmSlot::Slot3,
    //     ),
    //     TdmGpioConfig::new(false, false, false),
    // );

    let bclk = pins.gpio42;
    let din = pins.gpio45;
    let dout = pins.gpio39;
    let mclk = pins.gpio41.into();
    let ws = pins.gpio40;

    // let std_config = StdConfig::philips(24000, DataBitWidth::Bits16);

    // i2s_config
    let mut i2s_driver = I2sDriver::<I2sBiDir>::new_std_bidir(
        peripherals.i2s0,
        &std_config,
        bclk,
        din,
        dout,
        mclk,
        ws,
    )
    .unwrap();

    // let mut i2s_driver = I2sDriver::<I2sBiDir>::new_std_bidir(
    //     peripherals.i2s0,
    //     &std_config,

    // let mut i2s_driver =
    //     I2sDriver::<I2sTx>::new_std_tx(peripherals.i2s0, &std_config, bclk, dout, mclk, ws)
    //         .unwrap();

    // let mut i2s_tdm_rx = I2sDriver::new_tdm_rx(peripherals.i2s0, &tdm_config, bclk, din, mclk, ws);

    i2s_driver.tx_enable().unwrap();
    i2s_driver.rx_enable().unwrap();
    // let mut i2s_driver =
    //     I2sDriver::new_std_tx(peripherals.i2s0, &std_config, bclk, dout, mclk, ws).unwrap();

    // I2sDriver::new_tdm_rx(peripherals.i2s0, std_config, bclk, din, mclk, ws);
    let i2s_driver_arc = Arc::new(Mutex::new(i2s_driver));
    let shared_state_arc = Arc::new(SharedAudioState::new());

    let i2s_clone_for_recorder = Arc::clone(&i2s_driver_arc);
    let i2s_clone_for_player = Arc::clone(&i2s_driver_arc);
    let state_clone_for_recorder = Arc::clone(&shared_state_arc);

    println!("初始化I2S完成！");

    match es8311.enable() {
        Ok(_) => {
            println!("成功启动音频解码器");
        }
        Err(e) => {
            println!("启动音频解码器失败:{:?}", e);
            return Err(anyhow!("启动音频解码器失败:{:?}", e));
        }
    }

    es8311.set_voice_volume(50)?;
    // play_audio(i2s_clone_for_player.lock().unwrap());

    let mut es7210 = Es7210::new(es7210_i2c_proxy);
    info!("初始化ES7210...");
    es7210.open()?;
    info!("enable ES7210...");
    es7210.enable()?;

    //  定时熄屏
    let once_timer = EspTaskTimerService::new()
        .unwrap()
        .timer(move || {
            channel_led.set_duty(0).unwrap();
            info!("One-shot timer triggered");
            channel_led.set_duty(max_duty).unwrap(); //关闭屏幕背光。
        })
        .unwrap();

    once_timer.after(Duration::from_secs(2)).unwrap();

    // log::info!("[Audio Task] play_audio start.");
    // play_audio(i2s_clone_for_player.lock().unwrap());
    // log::info!("[Audio Task] play_audio finished.");
    play_p3_audio(i2s_clone_for_player.lock().unwrap());

    // 1. 创建一个用于发送控制命令的Channel
    let (cmd_sender, cmd_receiver) = unbounded::<AudioCommand>();
    // let (cmd_sender, cmd_receiver): (Sender<AudioCommand>, Receiver<AudioCommand>) = channel();

    // ===============================================================
    // == 2. 启动一个独立的“音频处理”后台任务
    // ===============================================================
    let i2s_clone = Arc::clone(&i2s_driver_arc);
    let state_clone = Arc::clone(&shared_state_arc);

    // let notification = Arc::new(Notification::new());
    // let notifier = Arc::clone(&notification);
    // let notifier2 = Arc::clone(&notification);

    thread::spawn(move || {
        let mut is_recording = false;
        loop {
            // a. 检查是否有新的控制命令进来 (非阻塞)
            if let Ok(command) = cmd_receiver.try_recv() {
                match command {
                    AudioCommand::StartRecording => {
                        log::info!("[Audio Task] Received StartRecording command.");
                        // 清空旧缓冲区，准备录音
                        state_clone.buffer.lock().unwrap().clear();
                        is_recording = true;
                    }
                    AudioCommand::StopAndPlayback => {
                        log::info!("[Audio Task] Received StopAndPlayback command.");
                        is_recording = false;

                        // --- 修正后的播放逻辑 ---
                        let mut buffer_guard = state_clone.buffer.lock().unwrap();

                        if !buffer_guard.is_empty() {
                            log::info!("[Audio Task] Playing back {} bytes...", buffer_guard.len());
                            let i2s_clone_for_player2 = Arc::clone(&i2s_driver_arc);
                            let mut i2s_guard = i2s_clone_for_player2.lock().unwrap();
                            // VecDeque 提供了 as_slices() 方法，它返回一或两个连续的内存切片
                            let (slice1, slice2) = buffer_guard.as_slices();
                            // for frame in slice1 {
                            //     info!("[Audio Task] Playback frame: {:08b}", frame);
                            // }
                            // 播放第一个切片
                            if let Err(e) = i2s_guard.write_all(slice1, BLOCK) {
                                log::error!("[Audio Task] Playback failed on slice1: {:?}", e);
                            } else {
                                // 如果有第二个切片，继续播放
                                if !slice2.is_empty() {
                                    // for frame in slice2 {
                                    //     info!("[Audio Task] Playback frame: {:08b}", frame);
                                    // }
                                    if let Err(e) = i2s_guard.write_all(slice2, BLOCK) {
                                        log::error!(
                                            "[Audio Task] Playback failed on slice2: {:?}",
                                            e
                                        );
                                    }
                                }
                            }

                            log::info!("[Audio Task] Playback finished.");
                            buffer_guard.clear(); // 清空缓冲区
                        }

                        // // --- 执行播放逻辑 ---
                        // let playback_data: Vec<u8>;
                        // {
                        //     let mut buffer_guard = state_clone.buffer.lock().unwrap();
                        //     playback_data = buffer_guard.iter().cloned().collect();
                        //     buffer_guard.clear();
                        // }

                        // if !playback_data.is_empty() {
                        //     log::info!(
                        //         "[Audio Task] Playing back {} bytes...",
                        //         playback_data.len()
                        //     );
                        //     let mut i2s_guard = i2s_clone.lock().unwrap();
                        //     if let Err(e) = i2s_guard.write_all(&playback_data, BLOCK) {
                        //         log::error!("[Audio Task] Playback failed: {:?}", e);
                        //     } else {
                        //         log::info!("[Audio Task] Playback finished.");
                        //     }
                        // }
                    }
                }
            }

            // b. 如果当前处于录音状态，就持续读取数据
            if is_recording {
                const READ_CHUNK_SIZE: usize = 1024;
                let mut read_buffer = vec![0u8; READ_CHUNK_SIZE];
                let mut i2s_guard = i2s_clone.lock().unwrap();
                if let Ok(bytes_read) = i2s_guard.read(&mut read_buffer, 50) {
                    info!("bytes read from I2S : {} ", bytes_read);
                    if bytes_read > 0 {
                        state_clone
                            .buffer
                            .lock()
                            .unwrap()
                            .extend(&read_buffer[..bytes_read]);
                    }
                } else {
                    info!("I2Stream: Error reading I2S");
                }
            } else {
                // 如果不录音，就短暂休眠，避免CPU空转
                thread::sleep(Duration::from_millis(20));
            }
        }
    });
    log::info!("Background audio processing task started.");

    // handle.join().unwrap();
    log::info!("Background audio processing task started.");

    // //  定时熄屏
    // let once_timer2 = EspTaskTimerService::new()
    //     .unwrap()
    //     .timer(move || {
    //         info!("One-shot timer 2 triggered");
    //         cmd_sender.send(AudioCommand::StartRecording).unwrap();
    //     })
    //     .unwrap();

    // once_timer2.after(Duration::from_secs(2)).unwrap();

    let mut touch_button = Box::new(button::Button::new(pins.gpio0).unwrap());
    let mut volume_button = button::Button::new(pins.gpio47).unwrap();

    let mut application_state = ApplicationState::Idle;

    let mut app = Application::new();

    info!("Waiting for button press...");
    block_on(async move {
        // println!("Buttons initialized. Waiting for press...");
        let mut speaking = false;
        loop {
            select! {
                _ = touch_button.wait().fuse()  => {

                    if !speaking {

                        println!("touch_button 1 pressed!");
                        cmd_sender.send(AudioCommand::StartRecording).unwrap();

                        // let mut i2s_guard = i2s_clone_for_player.lock().unwrap();

                        // play_audio(i2s_guard);
                        // log::info!("[Audio Task] play_audio finished.");


                        speaking = true;
                        touch_button.enable_interrupt().unwrap();
                    } else {
                        println!("is speaking !");
                        // 发送“停止并播放”命令
                        cmd_sender.send(AudioCommand::StopAndPlayback).unwrap();
                        speaking = false;
                        log::info!("==> Action: Playback Recorded Audio");
                        touch_button.enable_interrupt().unwrap();
                    }
                }
                _ = volume_button.wait().fuse() => {
                    println!("volume_button 2 pressed!");
                    volume_button.enable_interrupt().unwrap();
                },
            }
        }
    });

    info!("Test complete. Entering infinite loop.");

    // loop {
    //     FreeRtos::delay_ms(1000);
    // }
    Ok(())
}

fn play_audio(mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>) {
    const PCM_DATA: &'static [u8] = include_bytes!("../assets/sound.pcm");

    info!(
        "Embedded PCM data size: {} bytes. Starting playback...",
        PCM_DATA.len()
    );

    // match i2s_driver.write_all(PCM_DATA, BLOCK) {
    //     Ok(_) => info!("Playback finished successfully!"),
    //     Err(e) => println!("I2S write error: {:?}", e),
    // }

    const CHUNK_SIZE: usize = 4096;

    info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

    // // 3. 使用 .chunks() 方法将整个PCM数据切分成多个小块
    for chunk in PCM_DATA.chunks(CHUNK_SIZE) {
        // 4. 逐块写入I2S驱动
        //    i2s_driver.write() 会阻塞，直到这一小块数据被成功写入DMA
        match i2s_driver.write(chunk, BLOCK) {
            Ok(bytes_written) => {
                // 打印一些进度信息，方便调试
                // info!("Successfully wrote {} bytes to I2S.", bytes_written);
            }
            Err(e) => {
                // 如果在写入过程中出错，打印错误并跳出循环
                info!("I2S write error on a chunk: {:?}", e);
                break;
            }
        }
    }
}

///
/// 播放p3格式的音频文件. <br/>
/// p3格式: [1字节类型, 1字节保留, 2字节长度, Opus数据]
///  
fn play_p3_audio(mut i2s_driver: MutexGuard<'_, I2sDriver<'_, I2sBiDir>>) {
    const P3_DATA: &'static [u8] = include_bytes!("../assets/activation.p3");

    info!(
        "Embedded p3 data size: {} bytes. Starting playback...",
        P3_DATA.len()
    );

    const CHUNK_SIZE: usize = 4096;

    info!("Starting playback in chunks of {} bytes...", CHUNK_SIZE);

    if P3_DATA.len() < 4 {
        error!("P3 data is too small to be valid.");
        return;
    }

    let p3_data_len = P3_DATA.len();
    info!("P3 data length: {} bytes", p3_data_len);

    let sample_rate = 16000; //# 采样率固定为16000Hz
    let channels = 1; //# 单声道
    let mut opus_decoder = OpusAudioDecoder::new(sample_rate, channels).unwrap();

    let mut offset = 0;

    while offset < p3_data_len {
        let len: [u8; 2] = P3_DATA[offset + 2..offset + 4].try_into().unwrap();
        let frame_len = u16::from_be_bytes(len) as usize;

        let opus_data = &P3_DATA[(offset + 4)..(offset + 4 + frame_len)];
        offset += 4 + frame_len;
        info!("offset {} bytes...", offset);

        // decoder = decoder.decode(sample_rate, channels);
        let decode_result = opus_decoder.decode(opus_data);

        match decode_result {
            Ok(pcm_data) => {
                //因为 p3文件是单声道的，而我们的 I2S 配置是双声道的，所以需要将单声道数据转换成双声道数据。
                let pcm_mono_data_len = pcm_data.len();

                let mut pcm_stereo_buffer = vec![0i16; pcm_mono_data_len * 2];

                // 2. 遍历单声道样本，并复制到立体声缓冲区的左右声道
                for i in 0..pcm_mono_data_len {
                    let sample = pcm_data[i];
                    pcm_stereo_buffer[i * 2] = sample; // 左声道
                    pcm_stereo_buffer[i * 2 + 1] = sample; // 右声道
                }

                let pcm_stereo_bytes: &[u8] = unsafe {
                    core::slice::from_raw_parts(
                        pcm_stereo_buffer.as_ptr() as *const u8,
                        pcm_stereo_buffer.len() * std::mem::size_of::<i16>(),
                    )
                };

                // 如果p3是双声道的，或者使用了单声道的 I2S 配置，我们就可以直接使用 decode 后的音频数据。
                // // 1. 首先，获取一个指向有效数据的切片
                // let pcm_slice: &[i16] = &pcm_data;

                // // 2. 使用unsafe块来进行零成本的类型转换
                // let pcm_bytes: &[u8] = unsafe {
                //     // a. 获取i16切片的裸指针和长度（以i16为单位）
                //     let ptr = pcm_slice.as_ptr();
                //     let len_in_i16 = pcm_slice.len();

                //     // b. 使用`core::slice::from_raw_parts`来创建一个新的字节切片
                //     //    - 将i16指针强制转换成u8指针
                //     //    - 将长度（以i16为单位）乘以每个i16的字节数（2），得到总的字节长度
                //     core::slice::from_raw_parts(
                //         ptr as *const u8,
                //         len_in_i16 * std::mem::size_of::<i16>(),
                //     )
                // };

                // // 3. 使用 .chunks() 方法将整个PCM数据切分成多个小块
                for chunk in pcm_stereo_bytes.chunks(CHUNK_SIZE) {
                    // 4. 逐块写入I2S驱动
                    //    i2s_driver.write() 会阻塞，直到这一小块数据被成功写入DMA
                    match i2s_driver.write(chunk, BLOCK) {
                        Ok(bytes_written) => {
                            // 打印一些进度信息，方便调试
                            info!("Successfully wrote {} bytes to I2S.", bytes_written);
                        }
                        Err(e) => {
                            // 如果在写入过程中出错，打印错误并跳出循环
                            info!("I2S write error on a chunk: {:?}", e);
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                info!("Opus decode error: {:?}", e);
                return;
            }
        }
    }
}

fn led_demo(led_pin: gpio::AnyOutputPin, channel: esp_idf_hal::rmt::CHANNEL0) {
    let mut ws2812 = WS2812RMT::new(led_pin, channel).unwrap();
    loop {
        info!("Red!");
        ws2812.set_pixel(rgb::RGB8::new(255, 0, 0)).unwrap();
        FreeRtos::delay_ms(1000);
        info!("Green!");
        ws2812.set_pixel(rgb::RGB8::new(0, 255, 0)).unwrap();
        FreeRtos::delay_ms(1000);
        info!("Blue!");
        ws2812.set_pixel(rgb::RGB8::new(0, 0, 255)).unwrap();
        FreeRtos::delay_ms(1000);
    }
}
