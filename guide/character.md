**繁體中文** · [English](character.en.md) · [日本語](character.ja.md) · [한국어](character.ko.md)

# 做你自己的桌面助理

AImate 的角色是一個資料夾。資料夾裡有圖層 PNG、`layers.json`（有哪些圖層、各放在哪裡）、`rig.json`（每個圖層怎麼動）和 `character.json`（她是誰、怎麼說話），還可以放她自己的聲音。把資料夾放進角色資料夾、重開桌寵，就能在設定裡換成她，不用重新建置。

小克是預設角色。看你想改多少，從下面挑一條路：

| 想改什麼 | 看哪幾節 | 需要什麼 |
| --- | --- | --- |
| 名字、個性、開場白、口頭禪 | 第 1 節 | 什麼都不用裝 |
| 換成另一張圖 | 第 2 到 6 節 | 能改圖的 AI、Python |
| 換聲音、口頭禪用她的聲音 | 第 7 節 | 本機語音（README 安裝的第 3 步） |
| 介面的顏色和圓角 | 第 9 節 | 什麼都不用裝 |

以下指令都在 PowerShell 裡、從 repo 根目錄執行。`%USERPROFILE%\.vrpet` 是桌寵的資料夾；設了環境變數 `VRPET_HOME` 的話，就是那個位置。

## 1. 只改個性：從小克複製一份

1. 按角色旁邊的「設定」，打開「角色」分頁。
2. 小克是內建角色，不能直接改。在「複製到我的資料夾」左邊的欄位填一個新的資料夾名稱，例如 `my-xiaoke`，再按按鈕。
3. 桌寵會換成複製出來的這一份。改「名字」「服裝」「個性」「開場白」「口頭禪」，按「存檔」。存檔後她馬上換成新的設定，不用重開。

「個性」就是給 Claude 的角色說明，最多 2000 字。固定的規則（她聽得到也會說話、怎麼做手勢）由程式補上，這裡只要寫她是誰、怎麼說話。

複製出來的資料夾在 `%USERPROFILE%\.vrpet\characters\my-xiaoke\`，裡面有小克的圖層、`layers.json`、`rig.json` 和 `character.json`。之後要換圖或換聲音，可以從這個資料夾開始改。

## 2. 準備五張圖

切圖腳本會把一張全身立繪切成 22 個能動的圖層，例如後髮、臉、每隻眼睛的眼白、虹膜和睫毛、閉眼、張嘴、前髮。它需要同一張圖的五個版本，都放在同一個工作資料夾裡：

| 檔案 | 內容 | 給改圖 AI 的指示 |
| --- | --- | --- |
| `master.png` | 母版：背景換成純洋紅 `#FF00FF`，去掉光暈，閉嘴、睜眼 | `brief_master.md` |
| `partmap.png` | 部件色塊圖：每個部件塗成一種純色，當作切圖的遮罩 | `brief_partmap.md` |
| `nofront.png` | 拿掉所有前髮，補出額頭和被頭髮蓋住的衣服 | `brief_nofront.md` |
| `backhair.png` | 只留後髮，把被頭和身體擋住的部分補完整 | `brief_backhair.md` |
| `face_states.png` | 閉眼加張嘴 | `brief_face.md` |

這五份指示在 `Scripts/live2d/`，是寫給能改圖的 AI 的英文提示。小克的五張是用 ChatGPT 的改圖功能做的。

1. 原圖存成 `source.png`，放進工作資料夾，例如 `%USERPROFILE%\aimate-art\hana\`。原圖要是正面、直立的全身圖，雙手稍微離開身體，雙腳分開。小克的原圖是 1024×1536 的直式圖。
2. `brief_master.md` 裡有一項在描述小克的外觀（頭髮、太陽髮夾、外套、百褶裙、靴子）。先把這一項改成你的角色。
3. 先做 `master.png`。其他四張都拿 `master.png` 當輸入圖來改，可以同時做。
4. 每一張都要「拿輸入圖來改」，不要讓 AI 重新畫一張。看得到的像素一律取自母版，其他四張只補被擋住的地方和會變化的部位，所以圖只要偏移幾個像素，圖層就對不齊。

色塊圖用 13 種顏色，對照表在 `brief_partmap.md`。角色沒有某個部件時，例如沒有髮飾，色塊圖裡就不要出現那個顏色。

## 3. 切圖層

第一次先裝 Python 套件：

```powershell
uv venv --python 3.11 .venv-art
uv pip install --python .venv-art\Scripts\python.exe psd-tools numpy pillow opencv-python-headless scipy
```

然後切圖。最後一行 `layers.py` 的第二個參數是角色資料夾，資料夾名稱 `hana` 就是角色的 id（規則見第 6 節）：

```powershell
$work = "$env:USERPROFILE\aimate-art\hana"
.venv-art\Scripts\python.exe Scripts\live2d\cut.py $work
.venv-art\Scripts\python.exe Scripts\live2d\layers.py $work "$env:USERPROFILE\.vrpet\characters\hana"
```

`cut.py` 去背，並檢查色塊圖和母版對不對得上。它印出的兩個數字，小克是 `silhouette IoU 0.9868` 和 `mean 0.64 px`。IoU 明顯比 0.98 低，或平均誤差有好幾個像素，表示色塊圖被重畫或移動過，要重做一張。

`layers.py` 把圖層 PNG 和 `layers.json` 寫進角色資料夾，再在工作資料夾留一份 `character.psd` 和 `rest_stack.png`。看這幾行：

- `shift nofront … backhair … face_states …`：三張補圖對母版的偏移。小克的都在 0.2 px 以內。超過 1 px，表示那張圖被移動過。
- `rest stack vs master: mean abs error`：所有圖層靜止時疊起來和母版差多少。小克是 0.59/255。
- `hair_clip is empty and left out; remove it from rig.json's layers`：這張圖沒有這個部件，所以沒有這個圖層。下一步要從 `rig.json` 拿掉它。

再打開 `rest_stack.png` 看一眼。它是所有圖層靜止時疊起來的樣子，看起來應該和母版一樣。

拆眼睛的顏色門檻是照小克的圖定的。換一張臉時，眼白、虹膜和睫毛可能拆錯，例如眼白只剩一條縫，桌寵上的虹膜就會被切掉。同一張臉改出來的第二套衣服可以用 `Scripts\live2d\reuse_eyes.py` 沿用第一套拆好的眼睛，用法寫在檔案開頭。

## 4. 寫 `rig.json`：讓圖層動起來

把小克的 `Pet\ui\characters\char-20261107\rig.json` 複製進角色資料夾。從第 1 節複製出來的資料夾已經有這個檔。

**`parameters` 一個字都不要改。** 點頭和手勢的動作數字（`Pet/src-tauri/src/cue/figure.json`）綁的是這張參數表：每個參數的 id、範圍、預設值，以及它們的順序。完全相同，新角色就有和小克一樣的點頭和手勢。只要改了一點，手勢就全部停用，只剩待機、眨眼、嘴型和看向游標。

要改的是軸心，也就是把小克身上的位置換成你的角色身上的同一個位置。座標是 `layers.json` 裡 `canvas` 上的像素，也就是 `master.png` 的像素，左上角是 (0, 0)。用任何看得到游標座標的看圖或繪圖軟體量。

| `rig.json` 裡的位置 | 是什麼 | 小克的值 |
| --- | --- | --- |
| `nodes` 的 `body` | 腰，身體搖擺的中心 | `[510, 600]` |
| `nodes` 的 `skirt` | 裙子擺動的中心 | `[510, 590]` |
| `nodes` 的 `head` | 脖子，頭轉動的中心 | `[507, 270]` |
| `nodes` 的 `armRight` | 畫面左邊那隻手的肩膀 | `[385, 335]` |
| `nodes` 的 `armLeft` | 畫面右邊那隻手的肩膀 | `[638, 335]` |
| `layers` 的 `back_hair` | 後髮擺動的支點，在頭頂 | `[512, 40]` |
| `layers` 的 `front_hair`、`hair_clip` | 前髮擺動的支點 | `[505, 20]` |
| `layers` 的 `eye_white_image_left`、`iris_image_left`、`eye_lash_image_left` | 畫面左邊那隻眼睛的中心 | `[462, 169]` |
| `layers` 的 `eye_white_image_right`、`iris_image_right`、`eye_lash_image_right` | 畫面右邊那隻眼睛的中心 | `[557, 167.5]` |
| `layers` 的 `mouth_open` | 嘴的中心，張嘴時從這裡上下拉開 | `[508, 224]` |
| `layers` 的 `leg_image_left`、`leg_image_right` | 大腿外側的上緣，雙腳併攏時的轉軸 | `[352, 866]`、`[666, 862]` |
| `look` 的 `at` | 兩眼之間，看向游標時從這裡看 | `[508, 170]` |

眼睛的中心可以直接用 `layers.py` 印出來的 `iris left: centre x,y` 和 `iris right: centre x,y`。

圖層名稱的 left 和 right 是畫面的左右。節點的名稱是角色自己的左右，所以 `armRight` 這個節點掛的是 `arm_image_left`，也就是畫面左邊那隻手。

每個 `drive` 寫的是「這個參數每一單位，讓它平移 `x`／`y` 個像素、旋轉 `rotate` 度、縮放 `sx`／`sy`」。手臂一單位是一度。先不用改，看她在桌面上動起來的樣子再調。

`layers.py` 拿掉的圖層，也要從 `rig.json` 的 `layers` 刪掉那一項。不刪的話，她畫不出來，原因是 `rig.json rigs hair_clip, which layers.json does not have`。

每個圖層都是一整片，只能平移、旋轉、縮放，不會像 Live2D 那樣變形。眨眼和張嘴是在兩張圖之間切換。

## 5. 寫 `character.json`

```json
{
  "name": "小花",
  "outfit": "制服",
  "persona": "你住在使用者的桌面角落，講話短、親切，用繁體中文回答。",
  "greeting": "你好，我是小花。",
  "voice": "voice.wav",
  "fillers": ["嗯……", "我想想"]
}
```

| 欄位 | 必要 | 限制 | 用在哪裡 |
| --- | --- | --- | --- |
| `name` | 是 | 1 到 32 字，不能有換行 | 設定清單、對話框的名牌、給 Claude 的說明 |
| `persona` | 是 | 1 到 2000 字 | 給 Claude 的角色說明 |
| `outfit` | 否 | 1 到 16 字，不能有換行或方括號 | 只顯示在設定清單，寫成「小花（制服）」，用來分辨同一個角色的不同衣服 |
| `greeting` | 否 | 最多 200 字，不能有方括號 | 桌寵啟動和換成她的時候說一次 |
| `voice` | 否 | 只能寫 `"voice.wav"`，資料夾裡要真的有這個檔 | 她的聲音，見第 7 節 |
| `fillers` | 否 | 最多 12 句，每句 1 到 40 字 | 她想答案時說的口頭禪，見第 7 節 |

不認得的欄位會被拒絕。打錯欄位名稱時，設定清單會直接寫出是哪一個，不會悄悄沒效果。

## 6. 放進去，換成她

1. 資料夾名稱就是角色的 id。只能用小寫英文、數字、`-` 和 `_`，第一個字是英文或數字，最長 48 字。不能和內建角色同名。
2. 資料夾放在 `%USERPROFILE%\.vrpet\characters\<id>\`。設定「角色」分頁的「打開角色資料夾」會直接打開這裡。
3. 檢查資料夾第一層有 `character.json`、`rig.json`、`layers.json`，以及 `layers.json` 列出的每一張 PNG。
4. 按「結束」關掉桌寵，再跑一次 `Scripts\start_pet.ps1`。
5. 打開設定的「角色」分頁，點她的名字。

桌寵會記住你的選擇，下次開啟還是她。

## 7. 她的聲音和口頭禪

這一節要用本機語音。沒裝本機語音也可以用角色，她只是不出聲音。

**聲音。** 把一段參考錄音存成 `voice.wav` 放進角色資料夾，並在 `character.json` 加上 `"voice": "voice.wav"`，再重開桌寵。語音服務會模仿這段錄音的聲音，不需要逐字稿。

- 用一個人的乾淨錄音，沒有背景音樂和雜音。小克的參考錄音是 22.7 秒。
- 錄音的音質會一起被模仿。電話音質的錄音，合成出來也會像電話。
- 檔案最大 10 MB。
- 只用你有權使用的聲音。不要在對方沒同意的情況下模仿真人的聲音。

**口頭禪。** 她在想答案時會先說一句口頭禪。`character.json` 寫好 `fillers` 後，在語音服務開著、角色資料夾已經放好的時候執行：

```powershell
.venv\Scripts\python.exe Scripts\tts_engines\render_fillers.py "$env:USERPROFILE\.vrpet\characters\hana"
```

它用她的聲音把每一句合成兩次，留下比較好的那一次，寫成 `fillers.json`。結果沒有經過語音辨識檢查，請自己聽過一遍。

她有自己的聲音、卻沒有 `fillers.json` 時，不會說口頭禪，因為預設的那組是小克的聲音。

## 8. 她沒出現或樣子不對

| 看到什麼 | 原因 | 怎麼辦 |
| --- | --- | --- |
| 設定清單裡，她那一行反灰，後面寫了一段原因 | 掃描時沒有通過檢查 | 照那段原因改，再重開桌寵 |
| 清單裡根本沒有她 | 資料夾不在 `characters` 裡，或還沒重開 | 用「打開角色資料夾」確認位置 |
| 她沒畫出來 | `rig.json` 和圖層對不上 | 看 `%USERPROFILE%\.vrpet\logs\pet.log` 裡 `SPIKE_REPORT` 那一行的 `"character"`，`"error"` 會寫原因 |
| 沒有點頭和手勢 | `rig.json` 的 `parameters` 和小克的不同 | `pet.log` 裡有 `gestures: <id> bound by cue/figure.json` 才是綁上了 |
| 文字有、聲音沒有 | 對話框會寫原因 | 常見的是 `character 'hana' has no voice.wav` |

反灰的常見原因：

- 資料夾名稱不合規則，例如有大寫或空白。
- 缺 `character.json`、`rig.json` 或 `layers.json`。
- `character.json` 讀不懂。欄位名稱打錯時，原因裡會有 `unknown field` 和打錯的名稱。
- `voice` 寫了 `voice.wav`，資料夾裡卻沒有。
- 檔案太大。上限是每張 PNG 16 MB、每份 JSON 1 MB（`fillers.json` 8 MB）、`voice.wav` 10 MB、整個資料夾 200 MB。
- 資料夾裡有捷徑（symlink 或 junction）。桌寵為了安全不跟隨捷徑，整個角色都不載入。

角色資料夾可能是別人做的、從網路下載的，所以桌寵只讀資料夾第一層的 PNG、JSON 和 WAV，每個檔案都檢查大小。「個性」的文字會交給 Claude，但對話核心的 Claude 沒有任何工具，碰不到你的檔案。

## 9. 介面主題

對話框的顏色、圓角和兩張小圖也可以換，做法和角色一樣：一個主題是一個資料夾，裡面有 `theme.json`。內建兩個主題：紙本（`paper`）和夜色（`night`）。

```json
{
  "name": "我的主題",
  "colors": {
    "accent": "#d97757",
    "paper": "#f3f2ef",
    "ink": "#2b2b2b",
    "border": "#cfc8bc",
    "controls": "#f3f2ef"
  },
  "radius": 18,
  "icon": "icon.svg",
  "paper_texture": "paper.svg"
}
```

| 欄位 | 必要 | 限制 | 用在哪裡 |
| --- | --- | --- | --- |
| `name` | 是 | 1 到 32 字 | 設定的「外觀」分頁 |
| `colors.accent` | 是 | `#rrggbb` | 滑過和輸入時的框線。她的名字是 accent 往 ink 調 40% |
| `colors.paper` | 是 | `#rrggbb` | 對話框、選項、對話紀錄的底色 |
| `colors.ink` | 是 | `#rrggbb` | 字的顏色 |
| `colors.border` | 是 | `#rrggbb` | 所有框線 |
| `colors.controls` | 是 | `#rrggbb` | 按鈕列、輸入框、麥克風按鈕的底色 |
| `radius` | 是 | 0 到 28 的整數，單位 px | 對話框的圓角 |
| `icon` | 否 | 資料夾裡的一個 `.svg` 或 `.png` | 對話框裡她名字旁邊的小圖示 |
| `paper_texture` | 否 | 資料夾裡的一個 `.svg` 或 `.png` | 鋪在底色上的紋理 |

1. 資料夾名稱的規則和角色一樣。放在 `%USERPROFILE%\.vrpet\themes\<id>\`，設定「外觀」分頁的「打開主題資料夾」會打開這裡。
2. 回到設定視窗，新的主題就在清單上，不用重開桌寵。點一下就換。

限制：

- 圖的檔名只能用小寫英文、數字、`_`、`.`、`-`。
- `theme.json` 最大 64 KB，每張圖最大 2 MB，整個資料夾最大 8 MB。
- 主題不能帶 CSS，只能改上面這五個顏色、圓角和兩張圖。
- 有問題的主題會反灰並寫出原因。選的主題壞掉時，桌寵先用紙本，修好就會回來。

## 10. 把角色做進安裝包

這一節是給要改 repo 本身的人。把角色資料夾放進 `Pet/ui/characters/<id>/`，在 `Pet/ui/characters/index.json` 的 `characters` 加上 id，再重新建置桌寵。主題是 `Pet/ui/themes/<id>/` 和 `Pet/ui/themes/index.json`。改完跑一次 `Scripts\run_pet_selftests.ps1`。
