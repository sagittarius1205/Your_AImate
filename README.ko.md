[繁體中文](README.md) · [English](README.en.md) · [日本語](README.ja.md) · **한국어**

# AImate

**AImate: 늘 쓰는 AI로 가상 캐릭터이자 가상 비서를 만들어 일을 도움받으세요**

AImate는 Windows 바탕 화면 한쪽에 사는 2D 어시스턴트입니다. 외모, 이름, 성격, 목소리를 모두 직접 디자인할 수 있습니다. 글을 입력하거나 마이크 버튼을 누른 채 말을 걸면, 캐릭터가 여러분 자신의 AI 계정으로 답을 생각하고 여러분의 컴퓨터에서 합성한 목소리로 말합니다. 기본으로 연결된 AI는 Claude(Claude Code를 통해)이며, 여러분 컴퓨터의 Ollama 모델로 바꿀 수도 있습니다.

앱의 화면은 번체 중국어로만 되어 있습니다. 이 문서에서는 버튼과 탭 이름을 앱에 표시되는 중국어 그대로 적고, 괄호 안에 한국어 뜻을 붙였습니다.

기본 캐릭터는 샤오커(小克)이며, 유니폼과 드레스 두 벌의 옷이 있습니다. 여러분만의 캐릭터로 바꾸고 싶다면 [캐릭터 가이드](guide/character.ko.md)를 따라 하세요.

<p align="center"><img src=".github/readme/xiaoke.png" alt="기본 캐릭터 샤오커의 두 벌의 옷: 유니폼과 드레스" width="400"></p>

<p align="center"><a href="https://ko-fi.com/Sagittarius1205"><img src="https://ko-fi.com/img/githubbutton_sm.svg" alt="Support me on Ko-fi"></a></p>

> 이 프로젝트는 개인 오픈 소스 프로젝트이며, Anthropic의 공식 제품이 아니고 Anthropic과 관계가 없습니다.

## 할 수 있는 일

- **대화.** 대화창은 비주얼 노벨 스타일입니다. 글을 입력하거나, 마이크 버튼을 누른 채 말하고 손을 떼면 전송됩니다.
- **여러분의 Claude로 답변.** 뒤에서 실행되는 것은 여러분 컴퓨터의 `claude` 명령이며, 여러분 자신의 로그인을 사용합니다. 이 프로젝트는 요금을 받지 않고, 서버도 없으며, Claude를 쓸 때는 여러분의 키에 손대지 않습니다. **設定**(설정)의 **模型**(모델) 탭에서 **Claude Haiku** 또는 **Claude Sonnet**을 고를 수 있습니다.
- **로컬 모델로도 답변.** 설정에서 대화 모델을 여러분 컴퓨터의 Ollama 모델로 바꿀 수 있습니다. 이때 여러분이 한 말은 컴퓨터 밖으로 나가지 않으며, 대화에 Claude Code도 필요 없습니다.
- **로컬 음성으로 말하기.** 음성은 VoxCPM2가 여러분의 그래픽 카드에서 합성하며, 입 모양이 소리에 맞춰 움직입니다. 캐릭터의 목소리는 텍스트 설명으로 합성한 것이며, 실제 사람의 녹음은 전혀 사용하지 않았습니다.
- **로컬에서 알아듣기.** 음성 인식은 SenseVoice를 사용하며 중국어, 영어, 일본어, 한국어, 광둥어를 지원합니다. 목소리는 여러분의 컴퓨터 밖으로 나가지 않습니다.
- **움직임.** 눈과 몸이 커서를 따라 돌아가고, 말할 때 손짓을 하며, 눈을 깜빡입니다.
- **직접 디자인.** 여러분만의 캐릭터로 바꿀 수 있습니다. 그림, 이름, 성격, 첫인사, 말버릇, 목소리를 모두 바꿀 수 있습니다. 인터페이스 테마도 직접 만들 수 있으며, **紙本**(종이)과 **夜色**(밤) 두 가지가 기본으로 들어 있습니다.

그래픽 카드가 없어도 사용할 수 있습니다. 음성 서비스가 켜져 있지 않으면 캐릭터는 글자만 표시하고 소리는 내지 않습니다.

## 필요한 것

| 항목 | 필수 | 설명 |
| --- | --- | --- |
| Windows 10 또는 11, 64비트 | 예 | 현재 Windows 11에서만 테스트했습니다 |
| [Rust](https://rustup.rs/)(stable, MSVC 툴체인) | 예 | Rust를 설치할 때 Visual Studio Build Tools의 "C++를 사용한 데스크톱 개발"도 함께 설치해야 합니다 |
| WebView2 Runtime | 예 | Windows 11에는 기본으로 들어 있습니다 |
| [Claude Code](https://code.claude.com/docs/en/overview) | 기본 설정에서 예 | 터미널에서 `claude`를 입력했을 때 로그인되고 답변해야 합니다. 대화 모델을 로컬 Ollama로 바꾸면 대화에는 필요 없습니다 |
| 마이크 | 아니요 | 음성 입력에만 필요합니다 |
| NVIDIA 그래픽 카드, VRAM 8 GB 이상 | 아니요 | 로컬 음성에만 필요합니다. 실측 사용량은 7.6 GB입니다 |
| Python 3.11과 [uv](https://docs.astral.sh/uv/) | 아니요 | 로컬 음성에만 필요합니다 |

디스크 공간: 음성 인식을 처음 빌드할 때 약 1.2 GB의 라이브러리를 내려받습니다. 음성 모델 VoxCPM2는 약 4.6 GB입니다.

## 설치

아래 명령은 모두 PowerShell에서 repo 루트 디렉터리를 기준으로 실행합니다.

### 1. 프로그램 세 개 빌드하기

```powershell
cargo build --manifest-path ChatCore\Cargo.toml
cargo build --manifest-path Stt\Cargo.toml -p vrstt
cargo build --manifest-path Pet\src-tauri\Cargo.toml
```

첫 번째는 대화 코어, 두 번째는 음성 인식, 세 번째는 데스크톱 펫 본체입니다. 데스크톱 펫은 `ChatCore\target`과 `Stt\target`에서 나머지 두 프로그램을 스스로 찾습니다.

### 2. 음성 인식 모델 내려받기

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\fetch_models.ps1
```

이 스크립트는 [sherpa-onnx의 release](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models)에서 파일 세 개(약 160 MB)를 내려받아 하나씩 SHA-256을 확인한 뒤 `%USERPROFILE%\.vrpet\stt\`에 넣습니다. 이미 내려받은 파일은 건너뜁니다.

### 3. 로컬 음성 설치하기(선택)

```powershell
uv venv --python 3.11 .venv
uv pip install --python .venv\Scripts\python.exe voxcpm soundfile scipy
uv pip install --python .venv\Scripts\python.exe --reinstall --index-url https://download.pytorch.org/whl/cu128 torch torchaudio
uv pip install --python .venv\Scripts\python.exe triton-windows
```

세 번째 줄은 생략할 수 없습니다. `voxcpm`을 설치할 때 의존 패키지가 CUDA 버전 torch를 CPU 버전으로 바꿔 버려서, 음성이 열 배 이상 느려집니다.

음성 서비스를 처음 시작하면 Hugging Face에서 VoxCPM2의 가중치(약 4.6 GB)를 내려받으므로 몇 분 기다려야 합니다. 그 뒤로는 시작할 때마다 로딩에 약 1분이 걸립니다.

Python을 `.venv`에 두고 싶지 않다면 환경 변수 `VRPET_PYTHON`을 그 `python.exe`의 전체 경로로 설정하세요.

## 실행

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\start_pet.ps1
```

스크립트는 먼저 백그라운드에서 음성 서비스를 켜고, 그다음 데스크톱 펫을 켭니다. 바탕 화면 바로 가기가 필요하면 `-Shortcut`을 붙여 한 번 실행하세요.

빌드가 끝나면 파일 탐색기에서 repo 루트의 `AImate.bat`을 두 번 클릭해 시작할 수도 있습니다. 같은 `Scripts\start_pet.ps1`을 백그라운드에서 실행하므로, 음성 서비스가 켜져 있지 않으면 그것도 먼저 켭니다. 아직 빌드하지 않았다면 창에 `AImate is not built yet`이 표시됩니다. 먼저 설치의 1단계에 따라 프로그램 세 개를 빌드하세요.

- 캐릭터 옆에 **設定**(설정), **結束**(종료), **對話**(대화) 버튼 세 개가 있습니다. **設定**(설정)에는 **角色**(캐릭터), **聲音**(목소리), **外觀**(외관), **開關**(스위치), **模型**(모델) 다섯 개의 탭이 있으며, **開關**(스위치)에서 컴퓨터를 켤 때 자동으로 시작하도록 설정할 수 있습니다.
- 로컬 Ollama를 쓰려면 **模型**(모델) 탭에서 **對話模型**(대화 모델)을 **本機或自訂（OpenAI 相容）**(로컬 또는 사용자 지정, OpenAI 호환)으로 바꿉니다. 주소의 기본값은 Ollama의 로컬 주소 `http://127.0.0.1:11434/v1`입니다. 데스크톱 펫이 이 주소에 설치된 모델을 물어보므로, **模型**(모델) 목록에서 하나를 고릅니다. 목록이 비어 있으면 먼저 `ollama pull`로 모델을 하나 설치한 뒤 **重新列出**(목록 새로 고침)을 누릅니다. 이 방식에서는 캐릭터가 최근 8번 주고받은 대화만 기억합니다. 주소에는 다른 OpenAI 호환 서비스의 `https://` 주소도 넣을 수 있습니다. 키는 환경 변수에 넣고, **金鑰的環境變數**(키의 환경 변수)에는 변수 이름만 적습니다. 원격 서비스를 쓰면 여러분이 하는 모든 말이 그 서비스로 전송됩니다. 지금까지는 Ollama로만 검증했습니다.
- **結束**(종료)를 누르면 데스크톱 펫, 대화 코어, 음성 인식이 꺼집니다. 음성 서비스는 남아 있으므로 다음에 시작할 때 모델 로딩을 다시 기다리지 않아도 됩니다.

## 나만의 캐릭터 디자인하기

캐릭터 하나는 폴더 하나입니다. `%USERPROFILE%\.vrpet\characters\`에 넣고 데스크톱 펫을 다시 시작하면 **設定**(설정)에서 그 캐릭터로 바꿀 수 있으며, 다시 빌드할 필요는 없습니다.

- **이름과 성격만 바꾸기:** **設定**(설정)의 **角色**(캐릭터) 탭에서 **複製到我的資料夾**(내 폴더로 복사)를 누르고, 복사된 사본을 고친 뒤 **存檔**(저장)을 누릅니다.
- **다른 그림으로 바꾸기:** 그림을 편집할 수 있는 AI로 같은 그림의 다섯 가지 버전을 만든 뒤, `Scripts\live2d\`의 스크립트로 레이어를 나눕니다.
- **목소리 바꾸기:** `voice.wav`를 하나 넣으면 캐릭터가 그 목소리로 말합니다.

전체 단계, 각 항목의 제한, 문제 해결은 [캐릭터 가이드](guide/character.ko.md)를 보세요. 인터페이스 테마를 만드는 방법도 같은 문서에 있습니다.

## 문제가 생겼을 때

로그는 `%USERPROFILE%\.vrpet\logs\`에 있습니다.

| 파일 | 내용 |
| --- | --- |
| `voice.log` | 음성 서비스. Python을 찾지 못하면 그 이유가 여기에 적힙니다 |
| `pet.log` | 데스크톱 펫 |

캐릭터가 글자만 표시하고 소리를 내지 않을 때는 대화창에 이유가 적힙니다. 가장 흔한 원인은 음성 서비스가 아직 모델을 로딩하고 있는 것입니다. 1분 정도 기다렸다가 다시 시도하세요.

자체 테스트:

```powershell
powershell -File Scripts\run_pet_selftests.ps1
Stt\target\debug\vrstt.exe selftest
.venv\Scripts\python.exe Scripts\tts_engines\serve.py --selftest
```

코드 주석에 나오는 `Docs/...`는 개발 중에 쓴 설계 노트이며, 현재 공개되어 있지 않습니다.

## 라이선스

- **코드:** MIT입니다. [LICENSE](LICENSE)를 보세요.
- **기본 캐릭터 샤오커의 그림과 목소리:** MIT가 아닙니다. 이 소프트웨어(수정한 버전 포함)와 함께 사용하고 배포할 수 있으며, 그 밖의 용도는 먼저 동의를 받아야 합니다. [LICENSE-ASSETS.md](LICENSE-ASSETS.md)를 보세요.
- **이 repo에 포함되지 않고 여러분이 직접 내려받는 모델:**
  - SenseVoice([FunASR](https://github.com/modelscope/FunASR), 알리바바 Tongyi Lab): FunASR Model Open Source License v1.1. 사용할 때 출처와 저작자를 밝혀야 합니다. 여기서 사용하는 것은 k2-fsa가 ONNX로 변환한 int8 버전입니다.
  - Silero VAD: MIT.
  - [VoxCPM2](https://huggingface.co/openbmb/VoxCPM2)(OpenBMB): Apache-2.0.
  - sherpa-onnx: Apache-2.0. 빌드할 때 내려받습니다.

## 후원

샤오커는 무료입니다. 샤오커가 도움이 되었다면 [Ko-fi에서 커피 한 잔 사 주세요](https://ko-fi.com/Sagittarius1205). 계속 만들어 갈 시간을 더 낼 수 있습니다.
