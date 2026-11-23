# Task: an eye map aligned to eyes_crop.png

`eyes_crop.png` in this folder is a close-up of an anime character's two eyes. It will be cut into
animation layers: the white of each eye, the iris that moves inside it, and the lashes over it. I
need a segmentation map of it: the SAME image, with every region repainted as one flat solid
colour, so I can use each colour as a cut-out mask.

Use the image generation tool to EDIT `eyes_crop.png` (it must be the input image). The map is a
mask, so geometry matters more than anything else: every outline must stay exactly where it is in
eyes_crop.png. Same canvas size, same framing, nothing moved, resized or redrawn.

Paint each region one flat colour. No shading, no outlines, no gradients, no texture.

| region | colour |
| --- | --- |
| the white of each eye: the whole visible opening between the upper lashes and the lower eyelid line, except the iris, including its shaded parts | #00FFFF cyan |
| each iris, with its pupil and the highlights in it, only the part that shows | #0000FF blue |
| the upper lashes and upper eyelid line, the lower eyelid line, and the eye corners | #000000 black |
| everything else: skin, hair, eyebrows, nose, strands of hair crossing the eyes | #FF00FF magenta |

Save it in this folder as `eyes_map.png`. Reply with only its absolute path and one line saying
whether eyes_crop.png was used as the input image (yes/no).
