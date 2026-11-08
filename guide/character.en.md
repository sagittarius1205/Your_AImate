[繁體中文](character.md) · **English** · [日本語](character.ja.md) · [한국어](character.ko.md)

# Make your own desktop assistant

An AImate character is a folder. The folder holds layer PNGs, `layers.json` (which layers exist and where each one sits), `rig.json` (how each layer moves) and `character.json` (who she is and how she talks). It can also hold her own voice. Put the folder in the characters folder and restart the pet. Then you can switch to her in the settings. No rebuild is needed.

The app's interface is in Traditional Chinese only. This guide shows each button, tab and field label exactly as the app shows it, followed by an English gloss, for example **設定** (Settings).

Xiaoke (小克) is the default character. Choose a path below, based on how much you want to change:

| What to change | Sections | What you need |
| --- | --- | --- |
| Name, personality, greeting, catchphrases | Section 1 | Nothing to install |
| A different image | Sections 2 to 6 | An image-editing AI, Python |
| A new voice, and catchphrases in her voice | Section 7 | Local voice (step 3 of the README install) |
| Interface colors and corner radius | Section 9 | Nothing to install |

Run all commands below in PowerShell, from the repo root. `%USERPROFILE%\.vrpet` is the pet's folder. If you set the environment variable `VRPET_HOME`, the folder is that location instead.

## 1. Change only the personality by copying Xiaoke

1. Click **設定** (Settings) next to the character, and open the **角色** (Character) tab.
2. Xiaoke is a built-in character, so you cannot edit her directly. In the field to the left of **複製到我的資料夾** (Copy to my folder), enter a new folder name, for example `my-xiaoke`. Then click the button.
3. The pet switches to the copy. Edit **名字** (Name), **服裝** (Outfit), **個性** (Personality), **開場白** (Greeting) and **口頭禪** (Catchphrases), then click **存檔** (Save). After you save, she uses the new settings at once. No restart is needed.

**個性** (Personality) is the character description given to Claude, up to 2000 characters. The program adds the fixed rules (she can hear and speak, and how she gestures). Here you only write who she is and how she talks.

The copied folder is at `%USERPROFILE%\.vrpet\characters\my-xiaoke\`. It holds Xiaoke's layers, `layers.json`, `rig.json` and `character.json`. Later, to change the image or the voice, you can start from this folder.

## 2. Prepare five images

The cutting script cuts one full-body illustration into 22 movable layers, for example the back hair, the face, the eye white, iris and lashes of each eye, closed eyes, open mouth, and front hair. It needs five versions of the same image, all in one working folder:

| File | Contents | Instructions for the image-editing AI |
| --- | --- | --- |
| `master.png` | Master: background replaced with pure magenta `#FF00FF`, glow removed, mouth closed, eyes open | `brief_master.md` |
| `partmap.png` | Part color map: each part painted in one solid color, used as the cutting mask | `brief_partmap.md` |
| `nofront.png` | All front hair removed, with the forehead and the clothes under the hair filled in | `brief_nofront.md` |
| `backhair.png` | Only the back hair, with the parts hidden by the head and body filled in | `brief_backhair.md` |
| `face_states.png` | Eyes closed and mouth open | `brief_face.md` |

These five instructions are in `Scripts/live2d/`. They are English prompts for an image-editing AI. Xiaoke's five images were made with the image-editing feature of ChatGPT.

1. Save the original image as `source.png` in the working folder, for example `%USERPROFILE%\aimate-art\hana\`. The original must be a front-facing, upright, full-body image, with the hands a little away from the body and the feet apart. Xiaoke's original is a 1024×1536 portrait image.
2. One item in `brief_master.md` describes Xiaoke's look (hair, sun hair clip, jacket, pleated skirt, boots). First change this item to describe your character.
3. Make `master.png` first. The other four all use `master.png` as the input image. You can make them at the same time.
4. For every image, have the AI edit the input image. Do not let the AI draw a new image. All visible pixels come from the master. The other four only fill in hidden areas and the parts that change. So if an image shifts by even a few pixels, the layers do not line up.

The color map uses 13 colors. The color table is in `brief_partmap.md`. If your character does not have a part, for example no hair accessory, do not use that color in the color map.

## 3. Cut the layers

The first time, install the Python packages:

```powershell
uv venv --python 3.11 .venv-art
uv pip install --python .venv-art\Scripts\python.exe psd-tools numpy pillow opencv-python-headless scipy
```

Then cut the image. In the last line, the second argument of `layers.py` is the character folder. The folder name `hana` is the character's id (see section 6 for the rules):

```powershell
$work = "$env:USERPROFILE\aimate-art\hana"
.venv-art\Scripts\python.exe Scripts\live2d\cut.py $work
.venv-art\Scripts\python.exe Scripts\live2d\layers.py $work "$env:USERPROFILE\.vrpet\characters\hana"
```

`cut.py` removes the background and checks that the color map lines up with the master. It prints two numbers. For Xiaoke they are `silhouette IoU 0.9868` and `mean 0.64 px`. If the IoU is clearly below 0.98, or the mean error is several pixels, the color map was redrawn or moved. Make it again.

`layers.py` writes the layer PNGs and `layers.json` into the character folder. It also leaves a `character.psd` and a `rest_stack.png` in the working folder. Check these lines:

- `shift nofront … backhair … face_states …`: the offset of the three fill images from the master. Xiaoke's are all within 0.2 px. More than 1 px means that image was moved.
- `rest stack vs master: mean abs error`: how much all the layers stacked at rest differ from the master. Xiaoke's is 0.59/255.
- `hair_clip is empty and left out; remove it from rig.json's layers`: this image does not have that part, so there is no layer for it. In the next step, remove it from `rig.json`.

Then open `rest_stack.png` and look at it. It shows all the layers stacked at rest. It should look the same as the master.

The color thresholds that split the eyes were set for Xiaoke's image. With a different face, the eye white, iris and lashes may split wrong. For example, if the eye white is only a thin slit, the iris on the pet gets cut off. For a second outfit made from the same face, `Scripts\live2d\reuse_eyes.py` can reuse the eyes already split for the first outfit. Usage is at the top of the file.

## 4. Write `rig.json` to make the layers move

Copy Xiaoke's `Pet\ui\characters\char-20261107\rig.json` into the character folder. The folder you copied in section 1 already has this file.

**Do not change anything in `parameters`.** The motion numbers for nods and gestures (`Pet/src-tauri/src/cue/figure.json`) are bound to this parameter table: the id, range and default value of each parameter, and their order. If the table is exactly the same, the new character gets the same nods and gestures as Xiaoke. If you change even a little, all gestures turn off. Only idle motion, blinking, mouth movement and looking at the cursor remain.

What you change are the pivots. Replace each position on Xiaoke with the same position on your character. Coordinates are pixels on the `canvas` in `layers.json`, which are the pixels of `master.png`. The top-left corner is (0, 0). Measure them in any image viewer or drawing program that shows cursor coordinates.

| Position in `rig.json` | What it is | Xiaoke's value |
| --- | --- | --- |
| `body` in `nodes` | Waist, the center of the body sway | `[510, 600]` |
| `skirt` in `nodes` | Center of the skirt swing | `[510, 590]` |
| `head` in `nodes` | Neck, the center of head turns | `[507, 270]` |
| `armRight` in `nodes` | Shoulder of the arm on the left side of the screen | `[385, 335]` |
| `armLeft` in `nodes` | Shoulder of the arm on the right side of the screen | `[638, 335]` |
| `back_hair` in `layers` | Pivot of the back hair swing, at the top of the head | `[512, 40]` |
| `front_hair`, `hair_clip` in `layers` | Pivot of the front hair swing | `[505, 20]` |
| `eye_white_image_left`, `iris_image_left`, `eye_lash_image_left` in `layers` | Center of the eye on the left side of the screen | `[462, 169]` |
| `eye_white_image_right`, `iris_image_right`, `eye_lash_image_right` in `layers` | Center of the eye on the right side of the screen | `[557, 167.5]` |
| `mouth_open` in `layers` | Center of the mouth. The open mouth stretches up and down from here | `[508, 224]` |
| `leg_image_left`, `leg_image_right` in `layers` | Upper edge of the outer thigh, the pivot when the feet come together | `[352, 866]`, `[666, 862]` |
| `at` in `look` | Between the eyes. She looks at the cursor from here | `[508, 170]` |

For the eye centers, you can use the `iris left: centre x,y` and `iris right: centre x,y` values that `layers.py` prints.

Left and right in layer names mean the left and right of the screen. Node names use the character's own left and right. So the node `armRight` holds `arm_image_left`, which is the arm on the left side of the screen.

Each `drive` gives, for each unit of this parameter, how many pixels to move it (`x`/`y`), how many degrees to rotate it (`rotate`), and how much to scale it (`sx`/`sy`). For the arms, one unit is one degree. You do not need to change these yet. Watch her move on the desktop first, then adjust.

If `layers.py` removed a layer, also delete that entry from `layers` in `rig.json`. If you do not, she cannot be drawn, and the reason is `rig.json rigs hair_clip, which layers.json does not have`.

Each layer is one solid piece. It can only move, rotate and scale. It does not deform the way Live2D does. Blinking and mouth opening switch between two images.

## 5. Write `character.json`

```json
{
  "name": "Hana",
  "outfit": "Uniform",
  "persona": "You live in a corner of the user's desktop. You talk in short, friendly sentences and answer in English.",
  "greeting": "Hi, I'm Hana.",
  "voice": "voice.wav",
  "fillers": ["Hmm...", "Let me think"]
}
```

| Field | Required | Limits | Where it is used |
| --- | --- | --- | --- |
| `name` | Yes | 1 to 32 characters, no line breaks | The settings list, the name tag in the dialogue box, the description given to Claude |
| `persona` | Yes | 1 to 2000 characters | The character description given to Claude |
| `outfit` | No | 1 to 16 characters, no line breaks or square brackets | Shown only in the settings list, as "Hana (Uniform)". It tells apart different outfits of the same character |
| `greeting` | No | Up to 200 characters, no square brackets | Said once when the pet starts and when you switch to her |
| `voice` | No | Must be exactly `"voice.wav"`, and the file must exist in the folder | Her voice. See section 7 |
| `fillers` | No | Up to 12 lines, each 1 to 40 characters | Catchphrases she says while she thinks of an answer. See section 7 |

Unknown fields are rejected. If you mistype a field name, the settings list names that field. The mistake does not fail silently.

## 6. Put her in and switch to her

1. The folder name is the character's id. Use only lowercase English letters, digits, `-` and `_`. The first character must be a letter or a digit. The maximum length is 48 characters. It cannot have the same name as a built-in character.
2. Put the folder at `%USERPROFILE%\.vrpet\characters\<id>\`. The **打開角色資料夾** (Open character folder) button on the **角色** (Character) tab in the settings opens this location.
3. Check that the top level of the folder has `character.json`, `rig.json`, `layers.json`, and every PNG that `layers.json` lists.
4. Click **結束** (Quit) to close the pet, then run `Scripts\start_pet.ps1` again.
5. Open the **角色** (Character) tab in the settings and click her name.

The pet remembers your choice. Next time it opens, she is still the character.

## 7. Her voice and catchphrases

This section needs local voice. Without local voice you can still use the character. She just makes no sound.

**Voice.** Save a reference recording as `voice.wav` in the character folder, add `"voice": "voice.wav"` to `character.json`, and restart the pet. The voice service imitates the voice in this recording. No transcript is needed.

- Use a clean recording of one person, with no background music or noise. Xiaoke's reference recording is 22.7 seconds.
- The audio quality of the recording is imitated too. If the recording has phone quality, the synthesized speech also sounds like a phone.
- The file can be at most 10 MB.
- Only use a voice that you have the right to use. Do not imitate a real person's voice without their consent.

**Catchphrases.** While she thinks of an answer, she first says a catchphrase. After you write `fillers` in `character.json`, run this while the voice service is running and the character folder is in place:

```powershell
.venv\Scripts\python.exe Scripts\tts_engines\render_fillers.py "$env:USERPROFILE\.vrpet\characters\hana"
```

It synthesizes each line twice in her voice, keeps the better one, and writes the results to `fillers.json`. Speech recognition does not check the results. Listen to them yourself once.

If she has her own voice but no `fillers.json`, she does not say catchphrases, because the default set is in Xiaoke's voice.

## 8. She does not appear or looks wrong

| What you see | Cause | What to do |
| --- | --- | --- |
| In the settings list, her row is grayed out with a reason after it | She failed a check during the scan | Fix what the reason says, then restart the pet |
| She is not in the list at all | The folder is not in `characters`, or you have not restarted yet | Use **打開角色資料夾** (Open character folder) to check the location |
| She is not drawn | `rig.json` does not match the layers | In `%USERPROFILE%\.vrpet\logs\pet.log`, find `"character"` in the `SPIKE_REPORT` line. `"error"` gives the reason |
| No nods or gestures | `parameters` in `rig.json` differ from Xiaoke's | The gestures are bound only if `pet.log` has `gestures: <id> bound by cue/figure.json` |
| Text but no sound | The dialogue box shows the reason | A common one is `character 'hana' has no voice.wav` |

Common reasons for a grayed-out row:

- The folder name breaks the rules, for example it has capital letters or spaces.
- `character.json`, `rig.json` or `layers.json` is missing.
- `character.json` cannot be parsed. If a field name is mistyped, the reason includes `unknown field` and the mistyped name.
- `voice` is set to `voice.wav`, but the folder has no such file.
- A file is too large. The limits are 16 MB per PNG, 1 MB per JSON file (8 MB for `fillers.json`), 10 MB for `voice.wav`, and 200 MB for the whole folder.
- The folder contains a shortcut (symlink or junction). For safety, the pet does not follow shortcuts, and it does not load the character at all.

Someone else may have made a character folder, or you may have downloaded it from the internet. So the pet reads only the PNG, JSON and WAV files at the top level of the folder, and it checks the size of each file. The **個性** (Personality) text goes to Claude, but the Claude in the chat core has no tools and cannot touch your files.

## 9. Interface themes

You can also change the colors, the corner radius and the two small images of the dialogue box. It works the same way as characters. A theme is a folder that holds a `theme.json`. Two themes are built in, paper (`paper`) and night (`night`).

```json
{
  "name": "My theme",
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

| Field | Required | Limits | Where it is used |
| --- | --- | --- | --- |
| `name` | Yes | 1 to 32 characters | The **外觀** (Appearance) tab in the settings |
| `colors.accent` | Yes | `#rrggbb` | Borders on hover and while typing. Her name uses accent shifted 40% toward ink |
| `colors.paper` | Yes | `#rrggbb` | Background color of the dialogue box, the options and the chat history |
| `colors.ink` | Yes | `#rrggbb` | Text color |
| `colors.border` | Yes | `#rrggbb` | All borders |
| `colors.controls` | Yes | `#rrggbb` | Background color of the button bar, the input box and the microphone button |
| `radius` | Yes | Integer from 0 to 28, in px | Corner radius of the dialogue box |
| `icon` | No | One `.svg` or `.png` in the folder | Small icon next to her name in the dialogue box |
| `paper_texture` | No | One `.svg` or `.png` in the folder | Texture laid over the background color |

1. Folder names follow the same rules as characters. Put the folder at `%USERPROFILE%\.vrpet\themes\<id>\`. The **打開主題資料夾** (Open theme folder) button on the **外觀** (Appearance) tab in the settings opens this location.
2. Go back to the settings window. The new theme is in the list. You do not need to restart the pet. Click it to switch.

Limits:

- Image file names can use only lowercase English letters, digits, `_`, `.` and `-`.
- `theme.json` can be at most 64 KB, each image at most 2 MB, and the whole folder at most 8 MB.
- A theme cannot include CSS. It can only change the five colors above, the corner radius and the two images.
- A theme with problems is grayed out and shows the reason. If the selected theme breaks, the pet uses paper for now and switches back once you fix it.

## 10. Build a character into the installer

This section is for people who change the repo itself. Put the character folder in `Pet/ui/characters/<id>/`, add the id to `characters` in `Pet/ui/characters/index.json`, and rebuild the pet. For themes, use `Pet/ui/themes/<id>/` and `Pet/ui/themes/index.json`. After the change, run `Scripts\run_pet_selftests.ps1` once.
