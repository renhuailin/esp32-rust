# 开发日志

---

## 2026-08-18 修复播放时偶发"嗒嗒嗒"卡死（整机无响应）

### 现象

设备播放 TTS 音频期间，**偶发**（不是每次）出现以下症状组合，只能断电重启：

1. 扬声器持续"嗒嗒嗒嗒"响，无法停止；
2. 设备完全无响应（按键失效，无法停止播放）；
3. 服务器端音频仍在源源不断下发（websocket 连接正常）。

### 排查过程

#### 1. 从症状组合提取关键线索

三个症状看似矛盾（设备"死了"但网络"活着"），恰好构成精确的诊断依据：

| 症状 | 推论 |
|---|---|
| 嗒嗒声不停 | I2S TX DMA 在无人喂数据的状态下循环输出最后的残留 buffer（underrun） |
| 整机无响应 | **主循环线程**卡死（按键事件、websocket 消息都由它处理） |
| 服务器音频仍在下发 | websocket 收包是独立线程，不受主循环卡死影响 |

→ 问题定位为：某线程持锁永久阻塞，把主循环锁死了。

#### 2. 梳理播放链路与锁的持有关系

播放链路（会话模式）：

```
websocket 收包线程 → 主循环(AudioPacketReceived) → audio_decode_queue(入队)
audio_loop 线程 → start_audio_output() → mem::take(队列) → decode_opus_audio()
                → codec.lock() → output_data() → i2s write(chunk, BLOCK)
```

审读 `xiaozhi_audio_codec.rs` 时发现致命写法：

```rust
// output_data / test_play_pcm
i2s_driver.lock().unwrap().write(chunk, BLOCK)   // BLOCK = portMAX_DELAY，永久阻塞！
```

#### 3. 还原死锁链条

```
① 播放中 I2S TX DMA 偶发停摆（诱因，偶发所以"有时候"才出现）
② audio_loop: output_data() 里 write(chunk, BLOCK) 永久阻塞
   → audio_loop 卡死，codec 锁被永久持有
③ 主循环收到 tts stop → play_silence() → codec.lock() 拿不到锁
   → 主循环永久卡死 → 按键/停止全部失灵 = "设备卡死"
④ DMA 停摆前最后的残留 buffer 循环输出 = "嗒嗒嗒"不停
⑤ websocket 收包线程独立存活 = "服务器端还有音频过来"
```

连锁放大机制：**单条 write 的无限阻塞 → 线程卡死 → 锁被永久持有 → 锁死主循环 → 整机瘫痪**。

#### 4. 附带发现的隐患

`read_audio_data` 的读超时写的是 `1000` ticks。本项目 `CONFIG_FREERTOS_HZ` 使用 IDF 默认值 **100Hz**（`sdkconfig.defaults` 里 1000Hz 被注释掉了），1 tick = 10ms，即 **1000 ticks = 10 秒**——远超预期（写代码的人大概率以为是 1000ms）。RX 若卡住，audio_loop 会持 codec 锁 10 秒，播放随之断流。

### 修复

`src/audio/codec/xiaozhi_audio_codec.rs`：

1. 新增 tick 换算（从 esp-idf-sys 生成的 bindings 读取 `configTICK_RATE_HZ`，不硬编码）：

```rust
const fn ms_to_ticks(ms: u32) -> u32 {
    let t = ms * esp_idf_sys::configTICK_RATE_HZ / 1000;
    if t < 1 { 1 } else { t }
}
const I2S_WRITE_TIMEOUT_TICKS: u32 = ms_to_ticks(250);
const I2S_READ_TIMEOUT_TICKS: u32 = ms_to_ticks(500);
```

2. `output_data()` / `test_play_pcm()`：`BLOCK` → `I2S_WRITE_TIMEOUT_TICKS`（250ms）。
   超时即打 `I2S TX stalled` 错误日志并丢弃本包剩余数据，**尽快释放 codec 锁**。
3. `read_audio_data()`：`1000` ticks → `I2S_READ_TIMEOUT_TICKS`（500ms）。

效果对比：

| | TX 停摆一次的后果 |
|---|---|
| 修复前 | 整机死锁，只能断电重启 |
| 修复后 | 丢一个音频块（≤几百毫秒）+ `I2S TX stalled` 日志，设备继续可用 |

配合 2026-08-15 为 tts stop 增加的"排空等待 + 3 秒超时兜底"，整条播放链路已无永久阻塞点。

### 踩坑记录（编译层面）

- `esp_idf_sys` 的 bindings **没有**导出 `portTICK_PERIOD_MS`（只在 doc 注释里出现），但有 `configTICK_RATE_HZ`。换算用后者。
- `u32::max()` 不是 const fn，不能用在 `const` 定义里，需要手写 `const fn` 分支。

### 验证

烧录后正常使用，重点观察：

1. 长时间播放是否还出现嗒嗒响；
2. 若再出现，设备此时**是否还能响应按键**（应该能——这是本次修复的验证点）；
3. 日志中搜 `I2S TX stalled`：出现即命中真凶，且系统按设计降级而非死锁。

### 遗留问题

TX DMA 偶发停摆的底层诱因（AXP173 供电毛刺 / ES8311 / DMA 竞争）暂无法远程定位。
修复后再复现时日志会留下 `I2S TX stalled` 现场，届时可进一步追查。

---
