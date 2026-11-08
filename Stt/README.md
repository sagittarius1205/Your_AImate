# Stt：按住說話的本機語音輸入

按住麥克風按鈕說話，放開，文字出來。它是自己的行程，不認識對話核心。桌寵是它的一個消費者，不是它的容器。

## 怎麼跑

模型不進 repo。根目錄的 `Scripts\fetch_models.ps1` 會下載並核對它們；要手動放的話，把三個檔案放到 `%USERPROFILE%\.vrpet\stt\`（`VRPET_HOME` 換掉整個桌寵的資料夾，`VRSTT_MODELS` 只換模型的位置），路徑與 SHA-256 以 [models.json](models.json) 為準：

| 檔案 | 來源 |
| --- | --- |
| `packs/sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/model.int8.onnx` | k2-fsa `asr-models` release 的 `sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2` |
| `packs/sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/tokens.txt` | 同一個壓縮檔 |
| `vad/silero_vad.onnx` | k2-fsa `asr-models` release 的 `silero_vad.onnx` |

雜湊對不上、大小不對或檔案不在，服務會印一行 `{"ev":"error","kind":"model",...}` 說是哪個檔案，然後結束。它不會載入一個沒驗過的模型。

```bash
cd Stt && cargo build -p vrstt
./target/debug/vrstt.exe selftest          # 無頭測試，不需要麥克風
./target/debug/vrstt.exe file a.wav b.wav  # 每個檔案當成一次按壓，走同一個 engine
./target/debug/vrstt.exe probe 1           # 開一次預設麥克風，印裝置、原生格式與開裝置時間
./target/debug/vrstt.exe                   # 協定模式，桌寵用的就是這個
```

第一次建置時，`sherpa-onnx` crate 的 build script 會從 k2-fsa 的 GitHub release 下載預建的靜態函式庫（壓縮檔加解壓後共約 1.2 GB，快取在 `target/sherpa-onnx-prebuilt/`）。

## 協定

stdin 與 stdout 各是一行一個 JSON 物件。stdout 只有 `host/src/proto.rs` 寫，sherpa-onnx 自己的 log 走 stderr，`stdout_is_json` 這個測試起真的執行檔確認這件事。

進：`{"op":"press"}`、`{"op":"release"}`、`{"op":"cancel"}`、`{"op":"shutdown"}`。重複的指令是 no-op。stdin 關掉等於 `shutdown`，按住中的那一次會先結束、裝置先關。

出：

| `ev` | 欄位 | 什麼時候 |
| --- | --- | --- |
| `ready` | `version`、`model`、`vad`、`load_ms`、`capture`（`device`、`native_rate`、`native_channels`、`resampled`），找不到裝置時 `capture: null` 加 `capture_error` | 模型驗完、載完，一次 |
| `capture_started` | `session` | 按下，**在開裝置之前** |
| `transcript` | `session`、`seq`、`text`、`timings` | 放開，有字 |
| `nothing` | `session`、`why`（`no_speech`、`too_short`、`no_words`、`cancelled`、`device_lost`）、`timings` | 放開或取消，沒有字 |
| `error` | `session`（可為 null）、`kind`（`model`、`device`、`recognizer`、`protocol`）、`message` | 出錯。`model` 之後行程結束 |

`timings` 是 `open_ms`、`held_ms`、`speech_ms`（音訊時間）、`recognize_ms`、`release_to_text_ms`、`dropped_samples`、`glitches`。一次按壓最多一個 `transcript`，宿主用 `(session, seq)` 去重。

## 授權

程式碼 MIT。模型各自的授權，見根目錄 [README.md](../README.md) 的「授權」。
