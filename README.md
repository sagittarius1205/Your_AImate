**繁體中文** · [English](README.en.md) · [日本語](README.ja.md) · [한국어](README.ko.md)

# AImate

**AImate：把自己常用的AI打造虛擬人物、虛擬助理協助處理事情**

AImate 是住在 Windows 桌面角落的 2D 助理，長相、名字、個性和聲音都可以自己設計。你打字或按住麥克風跟她說話，她用你自己的 AI 帳號想答案，再用在你電腦上合成的聲音說出來。目前接的 AI 是 Claude（透過 Claude Code）。

預設角色是小克，有制服和禮服兩套衣服。想換成你自己的角色，照[角色教學](guide/character.md)做。

<p align="center"><img src=".github/readme/xiaoke.png" alt="預設角色小克的兩套衣服：制服和禮服" width="400"></p>

<p align="center"><a href="https://ko-fi.com/Sagittarius1205"><img src="https://ko-fi.com/img/githubbutton_sm.svg" alt="Support me on Ko-fi"></a></p>

> 這是個人的開源專案，不是 Anthropic 的官方產品，和 Anthropic 沒有關係。

## 她會做什麼

- **聊天。** 對話框是視覺小說的樣子。打字，或按住麥克風說話、放開送出。
- **用你的 Claude 回答。** 背後跑的是你電腦上的 `claude` 指令，用的是你自己的登入。這個專案不收費、沒有伺服器，也不碰你的金鑰。設定裡可以選 `haiku` 或 `sonnet`。
- **用本機的聲音說話。** 語音由 VoxCPM2 在你的顯示卡上合成，嘴型跟著聲音動。她的聲音是用文字描述合成出來的，沒有用任何真人的錄音。
- **在本機聽懂你。** 語音辨識用 SenseVoice，支援中、英、日、韓、粵語，聲音不會傳出你的電腦。
- **會動。** 眼睛和身體跟著游標轉，說話時有手勢，會眨眼。
- **自己設計。** 換成你自己的角色：圖、名字、個性、開場白、口頭禪、聲音都能換。介面主題也能自己做，內建紙本和夜色兩種。

沒有顯示卡也能用：語音服務沒開的時候，她只會顯示文字，不出聲音。

## 需要什麼

| 項目 | 必要 | 說明 |
| --- | --- | --- |
| Windows 10 或 11，64 位元 | 是 | 目前只在 Windows 11 上測過 |
| [Rust](https://rustup.rs/)（stable，MSVC 工具鏈） | 是 | 安裝 Rust 時要一起裝 Visual Studio Build Tools 的「使用 C++ 的桌面開發」 |
| WebView2 Runtime | 是 | Windows 11 已內建 |
| [Claude Code](https://code.claude.com/docs/en/overview) | 是 | 在終端機打 `claude` 要能登入並回答 |
| 麥克風 | 否 | 只有語音輸入要用 |
| NVIDIA 顯示卡，8 GB 以上顯存 | 否 | 只有本機語音要用，實測吃 7.6 GB |
| Python 3.11 和 [uv](https://docs.astral.sh/uv/) | 否 | 只有本機語音要用 |

硬碟空間：語音辨識第一次建置會下載約 1.2 GB 的函式庫；語音模型 VoxCPM2 約 4.6 GB。

## 安裝

以下指令都在 PowerShell 裡、從 repo 根目錄開始執行。

### 1. 建置三個程式

```powershell
cargo build --manifest-path ChatCore\Cargo.toml
cargo build --manifest-path Stt\Cargo.toml -p vrstt
cargo build --manifest-path Pet\src-tauri\Cargo.toml
```

第一個是對話核心，第二個是語音辨識，第三個是桌寵本身。桌寵會自己在 `ChatCore\target` 和 `Stt\target` 找另外兩個程式。

### 2. 下載語音辨識模型

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\fetch_models.ps1
```

這支腳本從 [sherpa-onnx 的 release](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models) 下載三個檔案（約 160 MB），逐一核對 SHA-256，放到 `%USERPROFILE%\.vrpet\stt\`。已經下載過的會跳過。

### 3. 安裝本機語音（選用）

```powershell
uv venv --python 3.11 .venv
uv pip install --python .venv\Scripts\python.exe voxcpm soundfile scipy
uv pip install --python .venv\Scripts\python.exe --reinstall --index-url https://download.pytorch.org/whl/cu128 torch torchaudio
uv pip install --python .venv\Scripts\python.exe triton-windows
```

第三行不能省。裝 `voxcpm` 時，相依套件會把 CUDA 版的 torch 換成 CPU 版，語音就會慢上十幾倍。

第一次啟動語音服務時，它會從 Hugging Face 下載 VoxCPM2 的權重（約 4.6 GB），要等幾分鐘。之後每次啟動載入約一分鐘。

Python 不想放在 `.venv` 的話，把環境變數 `VRPET_PYTHON` 設成那個 `python.exe` 的完整路徑。

## 執行

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\start_pet.ps1
```

腳本先在背景開語音服務，再開桌寵。要桌面捷徑的話，加上 `-Shortcut` 跑一次。

- 角色旁邊有「設定」「結束」「對話」三個按鈕。設定有角色、聲音、外觀、開關、模型五個分頁，「開關」裡可以設定開機時啟動。
- 「結束」會關掉桌寵、對話核心和語音辨識。語音服務會留著，下次啟動就不用再等模型載入。

## 設計你自己的角色

一個角色是一個資料夾，放進 `%USERPROFILE%\.vrpet\characters\` 再重開桌寵，就能在設定裡換成她，不用重新建置。

- **只改名字和個性：** 在設定的「角色」分頁按「複製到我的資料夾」，改複製出來的那一份，按「存檔」。
- **換成另一張圖：** 用能改圖的 AI 做出同一張圖的五個版本，再用 `Scripts\live2d\` 的兩支腳本切成圖層。
- **換聲音：** 放一段 `voice.wav`，她就用那個聲音說話。

完整的步驟、每個欄位的限制和排錯，見[角色教學](guide/character.md)。介面主題的做法也在同一份。

## 出問題的時候

紀錄在 `%USERPROFILE%\.vrpet\logs\`：

| 檔案 | 內容 |
| --- | --- |
| `voice.log` | 語音服務。找不到 Python 時，這裡會寫明原因 |
| `pet.log` | 桌寵 |

她只顯示文字、不出聲音時，對話框會寫原因。最常見的是語音服務還在載入模型，等一分鐘再試。

自我測試：

```powershell
powershell -File Scripts\run_pet_selftests.ps1
Stt\target\debug\vrstt.exe selftest
.venv\Scripts\python.exe Scripts\tts_engines\serve.py --selftest
```

程式碼註解裡提到的 `Docs/...` 是開發時的設計筆記，目前沒有公開。

## 授權

- **程式碼：** MIT，見 [LICENSE](LICENSE)。
- **預設角色小克的圖和聲音：** 不是 MIT。可以隨這個軟體（包括改過的版本）一起使用和散布，其他用途要先取得同意。見 [LICENSE-ASSETS.md](LICENSE-ASSETS.md)。
- **不在這個 repo 裡、由你自己下載的模型：**
  - SenseVoice（[FunASR](https://github.com/modelscope/FunASR)，阿里巴巴通義實驗室）：FunASR Model Open Source License v1.1，使用時要標明來源和作者。這裡用的是 k2-fsa 轉成 ONNX 的 int8 版本。
  - Silero VAD：MIT。
  - [VoxCPM2](https://huggingface.co/openbmb/VoxCPM2)（OpenBMB）：Apache-2.0。
  - sherpa-onnx：Apache-2.0，建置時下載。

## 贊助

小克是免費的。如果她對你有幫助，歡迎[在 Ko-fi 請我喝杯咖啡](https://ko-fi.com/Sagittarius1205)，這會讓我有更多時間做下去。
