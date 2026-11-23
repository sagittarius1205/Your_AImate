[繁體中文](README.md) · [English](README.en.md) · **日本語** · [한국어](README.ko.md)

# AImate

**AImate：いつも使っている AI を、用事を手伝ってくれるバーチャルキャラクター兼アシスタントに**

AImate は Windows のデスクトップの隅に住む 2D アシスタントです。見た目、名前、性格、声はすべて自分でデザインできます。文字を入力するか、マイクボタンを押したまま話しかけると、彼女はあなた自身の AI アカウントで答えを考え、あなたの PC 上で合成した声で話します。標準でつながる AI は Claude（Claude Code 経由）で、あなたの PC の Ollama のモデルに切り替えることもできます。

アプリの画面は繁体字中国語のみです。このドキュメントでは、ボタンや項目の名前を画面の表記のまま書き、後ろに日本語訳を括弧で添えています。

デフォルトのキャラクターは小克（シャオクー）で、制服とドレスの 2 着の衣装があります。自分のキャラクターに替えたい場合は、[キャラクター作成ガイド](guide/character.ja.md)の手順に従ってください。

<p align="center"><img src=".github/readme/xiaoke.png" alt="デフォルトキャラクター小克の 2 着の衣装：制服とドレス" width="400"></p>

<p align="center"><a href="https://ko-fi.com/Sagittarius1205"><img src="https://ko-fi.com/img/githubbutton_sm.svg" alt="Support me on Ko-fi"></a></p>

> これは個人のオープンソースプロジェクトです。Anthropic の公式製品ではなく、Anthropic とは関係ありません。

## できること

- **おしゃべり。** 会話ウィンドウはビジュアルノベル風です。文字を入力するか、マイクボタンを押したまま話し、離すと送信されます。
- **あなたの Claude で答える。** 裏で動いているのはあなたの PC にある `claude` コマンドで、あなた自身のログインを使います。このプロジェクトは料金を取らず、サーバーも持たず、Claude を使うときはあなたのキーにも触れません。設定の「模型」（モデル）タブで「Claude Haiku」か「Claude Sonnet」を選べます。
- **ローカルのモデルでも答える。** 設定で、会話モデルをあなたの PC の Ollama のモデルに切り替えられます。このとき、あなたが話した内容は PC の外に送られず、会話に Claude Code も必要ありません。
- **ローカルの声で話す。** 音声は VoxCPM2 があなたのグラフィックカード上で合成し、口の形が声に合わせて動きます。彼女の声は文章による説明から合成したもので、実在の人物の録音は一切使っていません。
- **ローカルで聞き取る。** 音声認識には SenseVoice を使います。中国語、英語、日本語、韓国語、広東語に対応し、音声があなたの PC の外に送られることはありません。
- **動く。** 目と体がカーソルの方を向き、話すときには身ぶりをし、まばたきもします。
- **自分でデザインできる。** 自分のキャラクターに替えられます。画像、名前、性格、あいさつ、口ぐせ、声をすべて変更できます。画面のテーマも自作でき、「紙本」（ペーパー）と「夜色」（ナイト）の 2 種類が内蔵されています。

グラフィックカードがなくても使えます。音声サービスが動いていないときは、文字を表示するだけで声は出ません。

## 必要なもの

| 項目 | 必須 | 説明 |
| --- | --- | --- |
| Windows 10 または 11、64 ビット | はい | 現在は Windows 11 でのみテストしています |
| [Rust](https://rustup.rs/)（stable、MSVC ツールチェーン） | はい | Rust をインストールするとき、Visual Studio Build Tools の「C++ によるデスクトップ開発」も一緒にインストールします |
| WebView2 Runtime | はい | Windows 11 には最初から入っています |
| [Claude Code](https://code.claude.com/docs/en/overview) | 標準では必要 | ターミナルで `claude` と入力し、ログインして回答が返ってくる状態にしておきます。会話モデルをローカルの Ollama にしたときは、会話には使いません |
| マイク | いいえ | 音声入力にだけ使います |
| NVIDIA グラフィックカード、VRAM 8 GB 以上 | いいえ | ローカル音声にだけ使います。実測で 7.6 GB を使用します |
| Python 3.11 と [uv](https://docs.astral.sh/uv/) | いいえ | ローカル音声にだけ使います |

ディスク容量：音声認識を初めてビルドするときに約 1.2 GB のライブラリをダウンロードします。音声モデル VoxCPM2 は約 4.6 GB です。

## インストール

以下のコマンドはすべて PowerShell で、repo のルートディレクトリから実行します。

### 1. 3 つのプログラムをビルドする

```powershell
cargo build --manifest-path ChatCore\Cargo.toml
cargo build --manifest-path Stt\Cargo.toml -p vrstt
cargo build --manifest-path Pet\src-tauri\Cargo.toml
```

1 つ目は会話コア、2 つ目は音声認識、3 つ目はデスクトップマスコット本体です。マスコットは `ChatCore\target` と `Stt\target` から残りの 2 つのプログラムを自動で探します。

### 2. 音声認識モデルをダウンロードする

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\fetch_models.ps1
```

このスクリプトは [sherpa-onnx の release](https://github.com/k2-fsa/sherpa-onnx/releases/tag/asr-models) から 3 つのファイル（約 160 MB）をダウンロードし、1 つずつ SHA-256 を確認してから `%USERPROFILE%\.vrpet\stt\` に置きます。ダウンロード済みのものはスキップします。

### 3. ローカル音声をインストールする（任意）

```powershell
uv venv --python 3.11 .venv
uv pip install --python .venv\Scripts\python.exe voxcpm soundfile scipy
uv pip install --python .venv\Scripts\python.exe --reinstall --index-url https://download.pytorch.org/whl/cu128 torch torchaudio
uv pip install --python .venv\Scripts\python.exe triton-windows
```

3 行目は省略できません。`voxcpm` をインストールすると、依存パッケージが CUDA 版の torch を CPU 版に置き換えてしまい、音声が十数倍遅くなります。

音声サービスを初めて起動すると、Hugging Face から VoxCPM2 の重み（約 4.6 GB）をダウンロードするため、数分かかります。2 回目以降は、起動のたびに読み込みに約 1 分かかります。

Python を `.venv` に置きたくない場合は、環境変数 `VRPET_PYTHON` にその `python.exe` のフルパスを設定してください。

## 実行

```powershell
powershell -ExecutionPolicy Bypass -File Scripts\start_pet.ps1
```

スクリプトはまず音声サービスをバックグラウンドで起動し、次にマスコットを起動します。デスクトップにショートカットが欲しい場合は、`-Shortcut` を付けて 1 回実行してください。

ビルドが済んだら、エクスプローラーで repo のルートにある `AImate.bat` をダブルクリックしても起動できます。同じ `Scripts\start_pet.ps1` をバックグラウンドで実行するので、音声サービスが動いていなければ先にそれも起動します。まだビルドしていない場合は、ウィンドウに `AImate is not built yet` と表示されます。先に「インストール」の手順 1 で 3 つのプログラムをビルドしてください。

- キャラクターの横に「設定」（設定）「結束」（終了）「對話」（会話）の 3 つのボタンがあります。設定には「角色」（キャラクター）「聲音」（音声）「外觀」（外観）「開關」（スイッチ）「模型」（モデル）の 5 つのタブがあり、「開關」（スイッチ）では PC の起動時に自動で起動する設定ができます。
- ローカルの Ollama を使うには、「模型」（モデル）タブで「對話模型」（会話モデル）を「本機或自訂（OpenAI 相容）」（ローカルまたはカスタム、OpenAI 互換）にします。アドレスの初期値は Ollama のローカルアドレス `http://127.0.0.1:11434/v1` です。マスコットがこのアドレスにインストール済みのモデルを問い合わせるので、「模型」（モデル）の一覧から 1 つ選びます。一覧が空なら、先に `ollama pull` でモデルを 1 つ入れてから「重新列出」（一覧を更新）を押します。この方法では、彼女は直近 8 往復の会話しか覚えていません。アドレスには、ほかの OpenAI 互換サービスの `https://` アドレスも入れられます。キーは環境変数に入れ、「金鑰的環境變數」（キーの環境変数）には変数の名前だけを入れます。リモートのサービスを使うと、あなたが話す一文一文がすべてそのサービスに送られます。今のところ Ollama でしか検証していません。
- 「結束」（終了）を押すと、マスコット、会話コア、音声認識が終了します。音声サービスは残るので、次回の起動時にモデルの読み込みを待たずに済みます。

## 自分のキャラクターをデザインする

キャラクター 1 体は 1 つのフォルダです。`%USERPROFILE%\.vrpet\characters\` に置いてマスコットを再起動すれば、設定でそのキャラクターに切り替えられます。ビルドし直す必要はありません。

- **名前と性格だけ変える：** 設定の「角色」（キャラクター）タブで「複製到我的資料夾」（自分のフォルダにコピー）を押し、コピーされた方を編集して「存檔」（保存）を押します。
- **別の画像に替える：** 画像を編集できる AI で同じ画像の 5 つのバージョンを作り、`Scripts\live2d\` にあるスクリプトでレイヤーに切り分けます。
- **声を替える：** `voice.wav` を 1 つ置くと、彼女はその声で話します。

詳しい手順、各項目の制限、トラブルシューティングは[キャラクター作成ガイド](guide/character.ja.md)を見てください。画面テーマの作り方も同じドキュメントにあります。

## 問題が起きたとき

ログは `%USERPROFILE%\.vrpet\logs\` にあります。

| ファイル | 内容 |
| --- | --- |
| `voice.log` | 音声サービス。Python が見つからないときは、ここに理由が書かれます |
| `pet.log` | デスクトップマスコット |

文字だけが表示されて声が出ないときは、会話ウィンドウに理由が表示されます。いちばん多いのは音声サービスがまだモデルを読み込んでいる場合で、1 分ほど待ってからもう一度試してください。

セルフテスト：

```powershell
powershell -File Scripts\run_pet_selftests.ps1
Stt\target\debug\vrstt.exe selftest
.venv\Scripts\python.exe Scripts\tts_engines\serve.py --selftest
```

コードのコメントに出てくる `Docs/...` は開発時の設計メモで、現在は公開していません。

## ライセンス

- **コード：** MIT です。[LICENSE](LICENSE) を見てください。
- **デフォルトキャラクター小克の画像と声：** MIT ではありません。このソフトウェア（改変したバージョンを含む）と一緒であれば使用・配布できます。それ以外の用途には事前の許可が必要です。[LICENSE-ASSETS.md](LICENSE-ASSETS.md) を見てください。
- **この repo に含まれず、あなた自身がダウンロードするモデル：**
  - SenseVoice（[FunASR](https://github.com/modelscope/FunASR)、Alibaba 通義実験室）：FunASR Model Open Source License v1.1。使用するときは出典と作者を明記する必要があります。ここで使っているのは、k2-fsa が ONNX に変換した int8 版です。
  - Silero VAD：MIT。
  - [VoxCPM2](https://huggingface.co/openbmb/VoxCPM2)（OpenBMB）：Apache-2.0。
  - sherpa-onnx：Apache-2.0。ビルド時にダウンロードされます。

## 支援

小克は無料です。もし役に立ったら、[Ko-fi でコーヒーを 1 杯おごってもらえる](https://ko-fi.com/Sagittarius1205)とうれしいです。開発を続ける時間が増えます。
