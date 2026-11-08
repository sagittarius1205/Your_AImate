# Task: a part map aligned to master.png

`master.png` in this folder is a full-body anime character on a flat magenta (#FF00FF) background.
It will be cut into Live2D layers. I need a segmentation map of it: the SAME image, with every
region repainted as one flat solid colour, so I can use each colour as a cut-out mask.

Use the image generation tool to EDIT `master.png` (it must be the input image). The map is a
mask, so geometry matters more than anything else: every outline must stay exactly where it is in
master.png. Same canvas size, same pose, same positions, nothing moved, resized or redrawn.

Paint each region one flat colour. No shading, no outlines, no gradients, no texture, no
anti-aliasing halos if you can avoid them. Left and right below mean the image's left and right.

| region | colour |
| --- | --- |
| background (everything that is magenta now) | #FF00FF magenta |
| hair in front of the face and the locks hanging in front of the shoulders | #FF0000 red |
| hair behind the head and body (the back mass visible beside the arms and torso) | #0000FF blue |
| sun hair clip | #FFFF00 yellow |
| face skin, ears and neck | #FFA040 light orange |
| both eyes (whites, irises, lashes) | #00FF00 green |
| both eyebrows | #008000 dark green |
| mouth | #00FFFF cyan |
| torso: jacket body, blouse, ribbon, belt | #808080 grey |
| arm on the image left: sleeve, cuff and hand | #800080 purple |
| arm on the image right: sleeve, cuff and hand | #FF8000 orange |
| skirt | #804000 brown |
| legs and boots | #000080 navy |

Save it in this folder as `partmap.png`. Reply with only its absolute path and one line saying
whether master.png was used as the input image (yes/no).
