[繁體中文](README.md) · **English** · [日本語](README.ja.md) · [한국어](README.ko.md)

# AImate

**AImate: turn the AI you already use into a virtual character and assistant that helps you get things done**

AImate is a 2D assistant that lives in a corner of your Windows desktop. You can design her look, name, personality and voice yourself. Type, or hold the microphone button and talk to her. She thinks of an answer with your own AI account, then says it in a voice synthesized on your computer. By default the AI is Claude (through Claude Code). You can also switch to a model in Ollama on your computer.

The default character is Xiaoke (小克). She has two outfits, a uniform and a formal dress. To switch to your own character, follow the [character guide](guide/character.en.md).

The app's interface is in Traditional Chinese only. This README shows each button and tab label exactly as the app shows it, followed by an English gloss, for example **設定** (Settings).

<p align="center"><img src=".github/readme/xiaoke.png" alt="Xiaoke, the default character, in her two outfits: a uniform and a formal dress" width="400"></p>

<p align="center"><a href="https://ko-fi.com/Sagittarius1205"><img src="https://ko-fi.com/img/githubbutton_sm.svg" alt="Support me on Ko-fi"></a></p>

> This is a personal open-source project. It is not an official Anthropic product and has no connection to Anthropic.

## What she does

- **Chat.** The dialogue box looks like a visual novel. Type, or hold the microphone button, speak, and release it to send.
- **Answers with your Claude.** Behind her runs the `claude` command on your computer, with your own login. This project charges nothing, has no server, and does not touch your keys when you use Claude. On the **模型** (Model) tab in the settings you can choose **Claude Haiku** or **Claude Sonnet**.
- **Can also use a local model.** In the settings you can switch the chat model to a model in Ollama on your computer. Then what you say does not leave your computer, and chatting does not need Claude Code.
- **Speaks with a local voice.** VoxCPM2 synthesizes the voice on your graphics card, and her mouth moves with the sound. Her voice was synthesized from a text description. No recording of a real person was used.
- **Understands you locally.** Speech recognition uses SenseVoice. It supports Chinese, English, Japanese, Korean and Cantonese. Your voice does not leave your computer.
- **Moves.** Her eyes and body turn to follow the cursor. She gestures while she talks, and she blinks.
- **Your own design.** Switch to your own character. You can change the image, name, personality, greeting, catchphrases and voice. You can also make your own interface themes. Two are built in, paper and night.

It also works without a graphics card. When the voice service is not running, she only shows text and makes no sound.

## What you need

| Item | Required | Notes |
| --- | --- | --- |
| Windows 10 or 11, 64-bit | Yes | Tested only on Windows 11 so far |
| [Rust](https://rustup.rs/) (stable, MSVC toolchain) | Yes | When you install Rust, also install "Desktop development with C++" from Visual Studio Build Tools |
| WebView2 Runtime | Yes | Built into Windows 11 |
| [Claude Code](https://code.claude.com/docs/en/overview) | By default | `claude` in a terminal must be able to log in and answer. When the chat model is local Ollama, chatting does not need it |
| Microphone | No | Only for voice input |
| NVIDIA graphics card, 8 GB or more VRAM | No | Only for local voice. Measured use is 7.6 GB |
| Python 3.11 and [uv](https://docs.astral.sh/uv/) | No | Only for local voice |

Disk space: speech recognition downloads about 1.2 GB of libraries on its first build. The voice model VoxCPM2 is about 4.6 GB.

## Install

Run all commands below in PowerShell, from the repo root.

### 1. Build the three programs

```powershell
cargo build --manifest-path ChatCore\Cargo.toml
cargo build --manifest-path Stt\Cargo.toml -p vrstt
cargo build --manifest-path Pet\src-tauri\Cargo.toml
```

The first is the chat core, the second is speech recognition, and the third is the desktop pet itself. The pet finds the other two programs in `ChatCore\target` and `Stt\target` by itself.

### 2. Download the speech recognition models

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\fetch_models.ps1
```

This script downloads three files (about 160 MB) from the [sherpa-onnx release](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models), checks the SHA-256 of each one, and puts them in `%USERPROFILE%\.vrpet\stt\`. It skips files that are already downloaded.

### 3. Install local voice (optional)

```powershell
uv venv --python 3.11 .venv
uv pip install --python .venv\Scripts\python.exe voxcpm soundfile scipy
uv pip install --python .venv\Scripts\python.exe --reinstall --index-url https://download.pytorch.org/whl/cu128 torch torchaudio
uv pip install --python .venv\Scripts\python.exe triton-windows
```

Do not skip the third line. When you install `voxcpm`, its dependencies replace the CUDA build of torch with the CPU build, and the voice becomes more than ten times slower.

The first time the voice service starts, it downloads the VoxCPM2 weights (about 4.6 GB) from Hugging Face. This takes a few minutes. After that, each start takes about one minute to load.

If you do not want Python in `.venv`, set the environment variable `VRPET_PYTHON` to the full path of that `python.exe`.

## Run

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\start_pet.ps1
```

The script first starts the voice service in the background, then starts the pet. For a desktop shortcut, run it once with `-Shortcut`.

Once it is built, you can also double-click `AImate.bat` at the repo root in File Explorer. It runs the same `Scripts\start_pet.ps1` in the background, so it also starts the voice service first when that is not running. If the pet is not built yet, the window says `AImate is not built yet`. Build the three programs in step 1 of the install first.

- Next to the character are three buttons: **設定** (Settings), **結束** (Quit) and **對話** (Chat). Settings has five tabs: **角色** (Character), **聲音** (Voice), **外觀** (Appearance), **開關** (Toggles) and **模型** (Model). In **開關** (Toggles) you can make the pet start when the computer starts.
- To use local Ollama instead, open the **模型** (Model) tab and set **對話模型** (Chat model) to **本機或自訂（OpenAI 相容）** (Local or custom, OpenAI-compatible). The address defaults to Ollama's local address `http://127.0.0.1:11434/v1`. The pet asks that address for the installed models, and you pick one in the **模型** (Model) list. If the list is empty, install a model with `ollama pull` first, then click **重新列出** (List again). In this mode she remembers only the last 8 turns of the conversation. The address can also be the `https://` address of another OpenAI-compatible service. Keep its key in an environment variable, and in **金鑰的環境變數** (Key's environment variable) enter only the variable's name. With a remote service, every sentence you say goes to that service. So far this has been verified only with Ollama.
- **結束** (Quit) closes the pet, the chat core and speech recognition. The voice service stays open, so the next start does not wait for the model to load again.

## Design your own character

A character is a folder. Put it in `%USERPROFILE%\.vrpet\characters\` and restart the pet. Then you can switch to her in the settings. No rebuild is needed.

- **Change only the name and personality:** On the **角色** (Character) tab in the settings, click **複製到我的資料夾** (Copy to my folder). Edit the copy, then click **存檔** (Save).
- **Use a different image:** Use an image-editing AI to make five versions of the same image. Then cut them into layers with the scripts in `Scripts\live2d\`.
- **Change the voice:** Add a `voice.wav`, and she speaks with that voice.

For the full steps, the limits of each field, and troubleshooting, see the [character guide](guide/character.en.md). The same guide also explains how to make interface themes.

## When something goes wrong

Logs are in `%USERPROFILE%\.vrpet\logs\`:

| File | Contents |
| --- | --- |
| `voice.log` | Voice service. If Python is not found, the reason is written here |
| `pet.log` | Desktop pet |

When she shows only text and makes no sound, the dialogue box shows the reason. The most common reason is that the voice service is still loading the model. Wait one minute and try again.

Self-tests:

```powershell
powershell -File Scripts\run_pet_selftests.ps1
Stt\target\debug\vrstt.exe selftest
.venv\Scripts\python.exe Scripts\tts_engines\serve.py --selftest
```

The `Docs/...` files that code comments mention are design notes from development. They are not public yet.

## License

- **Code:** MIT. See [LICENSE](LICENSE).
- **Image and voice of the default character Xiaoke:** Not MIT. You may use and distribute them with this software (including modified versions). Other uses need permission first. See [LICENSE-ASSETS.md](LICENSE-ASSETS.md).
- **Models that are not in this repo and that you download yourself:**
  - SenseVoice ([FunASR](https://github.com/modelscope/FunASR), Alibaba Tongyi Lab): FunASR Model Open Source License v1.1. When you use it, you must credit the source and the authors. This project uses the int8 ONNX version converted by k2-fsa.
  - Silero VAD: MIT.
  - [VoxCPM2](https://huggingface.co/openbmb/VoxCPM2) (OpenBMB): Apache-2.0.
  - sherpa-onnx: Apache-2.0, downloaded at build time.

## Support

Xiaoke is free. If she helps you, you are welcome to [buy me a coffee on Ko-fi](https://ko-fi.com/Sagittarius1205). This gives me more time to keep working on the project.
