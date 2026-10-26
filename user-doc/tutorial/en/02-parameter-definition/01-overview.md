English | [日本語](../../ja/02-parameter-definition/01-overview.md)

# 01. Overview and Recommended Parameters

## What you build in this tutorial

After you structure the model, you create the basic parameters that move the face, body, eyes, eyebrows, and mouth.  
This chapter aims for the following state:

- You can control face and body orientation separately as left-right, up-down, and tilt.
- You can control the eyes, eyebrows, and mouth, including left-right differences and expression variations.
- You can organize physics, such as hair and clothing, separately from the regular expression parameters.

## 1. Create parameters by what they mean, not by what they move

A parameter is not just an input that moves a Part.  
Even a single `Face::Yaw-Pitch` makes several elements follow slightly, such as the face Part, eyes, eyebrows, mouth, and hair roots.

Name parameters by meaning, as follows:

- `Face::Yaw-Pitch`  
  Turning the face left and right and up and down
- `Face::Roll`  
  Tilting the head to the left and right
- `Body::Yaw-Pitch`  
  Turning the body left and right and up and down
- `Body::Roll`  
  Tilting the body to the left and right
- `Eye::{L,R}::Blink`  
  Opening and closing of each eye, including expression-oriented closing
- `Eye::{L,R}::X-Y`  
  Gaze direction and relative position of the eye parts, for each eye
- `Eyebrow::{L,R}`  
  Vertical position of the eyebrows and expression changes
- `Mouth::Open`  
  Opening and closing of the mouth and expression changes

## 2. Choose between two-dimensional and one-dimensional parameters

Most orientations and expressions carry two meanings at once, so you build them as two-dimensional parameters.

- Handle left-right and up-down together  
  `Face::Yaw-Pitch`, `Body::Yaw-Pitch`, `Eye::{L,R}::X-Y`
- Handle opening and closing together with expression direction  
  `Eye::{L,R}::Blink`, `Mouth::Open`
- Handle vertical position together with expression direction  
  `Eyebrow::{L,R}`

A one-dimensional parameter is enough for anything that represents only tilt.

- `Face::Roll`
- `Body::Roll`

## 3. Decide these first

Before you create a parameter, decide the following three things:

- Name  
  Examples: `Face::Yaw-Pitch`, `Eye::L::Blink`
- Value range and default value  
  Examples: `-1..1` / `0..1`, default `0`
- What to register at which key  
  Examples: `(0,0)` is front, `(1,0)` is facing right, `(1,1)` is a combination correction

If you move on to deformation setup without deciding these, the meanings of "right", "up", "smile", and "anger" tend to get mixed within the same parameter.

## Next

- To decide value ranges and key positions  
  → [02. Value Ranges and Key Positions](./02-values-and-keys.md)
