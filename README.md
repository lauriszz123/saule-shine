# Shine2D

A Love2D-style 2D game engine for the [Saule](https://github.com/lauriszz123/saule)
language, and the reference consumer of `saule-sdk`.

```lua
import * from "shine"

Window.create(800, 600, "hello")
while Window.isOpen() do
    Window.pollEvents()
    local dt: float = Timer.getDelta()
    Graphics.clear(0.1, 0.1, 0.12)
    Graphics.setColor(1.0, 0.8, 0.2)
    Graphics.circle("fill", 400.0, 300.0, 32.0)
    Graphics.present()
end
```

Nothing here is linked into the Saule toolchain. This crate builds as a
`cdylib` (`saule_shine.so` / `.dll` / `.dylib`) that you drop into
`~/.saule/native_packages/`. **That one file is the whole package**: its
classes, every signature and every doc comment are compiled into the library,
and Saule reads them out of the file *without loading it* — so a program that
imports `shine` type-checks, completes and shows documentation in the editor
before a line of this code runs. There is no manifest to generate or keep in
sync.

All the ABI plumbing is handled by `saule-sdk`: each module exposes plain safe
functions annotated with `#[saule_export]`, and the package is declared with
`saule_package!`.

## Exposed classes

| Class      | Methods                                                   |
|------------|-----------------------------------------------------------|
| `Window`   | lifecycle, live size, title, position, focus, DPI — see below |
| `Graphics` | the UI drawing surface — see below                        |
| `Keyboard` | key state, per-frame edges, typed text, layouts — see below |
| `Mouse`    | position, buttons, per-frame edges, wheel, cursor, feel — see below |
| `Devices`  | `list`: the input devices of the machine — see below      |
| `Timer`    | `getTime`, `getDelta`, `getFPS`, `sleep`                   |
| `Clipboard`| `get`, `set`, `hasText`                                   |

Windowing uses `minifb` (X11-only on Linux, to stay compatible with WSLg).
Everything is drawn by a software rasterizer in `raster.rs` — there is no GPU
involved — with 4× sub-scanline antialiasing and nonzero-winding polygon fills.

## How the crate is laid out

Two halves, deliberately:

- **`render/`** owns everything drawable — render targets, the graphics state
  machine, the resource registries, and every `Graphics.*` operation. It
  touches no OS handle, so the whole drawing pipeline runs against plain pixel
  buffers.
- **`state/`** owns what genuinely needs a window: opening one, pumping its
  queue, latching input, and pacing the frame.
- **`gpu/`** puts frames on the window through OpenGL (ES 2.0, or desktop 2.1)
  where the machine has a GPU, and falls back to X11 shared memory or minifb
  where it hasn't. Drawing moves onto the GPU step by step:
  [docs/gpu.md](docs/gpu.md) has the plan. `SHINE_GPU=0` turns it off,
  `SHINE_TRACE_PRESENT=1` says which way frames go, `SHINE_GPU_DRAW=0`
  keeps drawing on the CPU while still presenting through the GPU
  (`SHINE_GPU_DRAW=1` draws through OpenGL even where it is only Mesa on
  the CPU, which on the bare screen Shine otherwise passes over for its
  own, faster drawing). A window inside a Shine display server draws on
  the GPU too, and on the CPU where the only OpenGL is Mesa's software
  one or `SHINE_APP_GPU=0` says so. On Intel's oldest chips (GMA 900 to
  3150) it does so on trial (`gpu/trial.rs`): a program that ends before
  it has shown a few frames has every program after it draw on the CPU,
  until the machine is started again. `SHINE_GPU_TRIAL=1` puts any chip
  on trial. `SHINE_TRACE_FRAMES=1` says every two seconds where a
  program's frames spend their time, and which drawing calls most of an
  average frame's drawing went to.
  `Graphics.getRenderer()` is the chip the bare screen is drawn by, as it
  names itself, or "" when the CPU draws, and then
  `Graphics.getRendererProblem()` is why: which step of starting OpenGL
  failed, or which of Shine's shaders the chip's driver turned down and
  what it said of each (every shader is tried, so one failure lists them
  all). Each of Shine's shaders is compiled in the one variant a draw
  needs (a blend mode, a kind of sampling: `gpu/program.rs`), which is
  what lets them run on chips with no branches and 64 instructions to a
  shader, Intel's GMA 3150 among them. And
  `SHINE_TRACE_FRAMES=1` reports, with the frame times, how many surfaces
  were copied between the CPU and the GPU.

The split is what makes the renderer testable. `Renderer::headless(w, h)` draws
into an offscreen surface with no display anywhere, so transforms, scissor
composition, canvas targeting, gradients and resource lifetime are covered by
ordinary unit tests that assert on pixels — none of which was reachable when
drawing required a live `minifb::Window`.

It is also why a frame allocates nothing. The renderer keeps its working
buffers (device-space paths, stroke outlines, the filler's coverage row, glyph
layout, wrapped lines) and rewinds them per call rather than freeing them, so
once they have grown to fit the largest shape a frame draws, drawing that frame
again allocates zero times. There is a test asserting exactly that.

## The `Graphics` API

A trimmed take on Love2D's `love.graphics`, keeping the parts that matter for
building user interfaces and dropping the 3D pipeline, meshes, particles and
video.

**Frame** — `clear([r,g,b,a])`, `present()`

**Shapes** — `rectangle(mode, x, y, w, h [, rx, ry])`, `circle(mode, x, y, r
[, segments])`, `ellipse(mode, x, y, rx, ry [, segments])`, `arc(mode, x, y, r,
a1, a2 [, arctype])`, `polygon(mode, points)`, `line(x1, y1, x2, y2)`,
`polyline(points)`, `points(points)`, `point(x, y)`

**Text** — `print(text, x, y)`, `printf(text, x, y, limit [, align])`,
`newFont(size [, path])`, `setNewFont(size [, path])`, `setFont`, `getFont`,
`getFontHeight()`, `getTextWidth(text)`

**State** — `setColor`/`getColor`, `setBackgroundColor`/`getBackgroundColor`,
`setLineWidth`/`getLineWidth`, `setLineStyle`/`getLineStyle`,
`setLineJoin`/`getLineJoin`, `setBlendMode`/`getBlendMode`,
`setDefaultFilter`/`getDefaultFilter`, `reset()`

**Clipping** — `setScissor([x,y,w,h])`, `intersectScissor(x,y,w,h)`,
`getScissor()`

**Canvases & images** — `newCanvas(w, h)`, `newImage(path)`,
`newImageFromBase64(data)`, `setCanvas([handle])`, `getCanvas()`,
`imageSize(handle)`, `draw(handle, x, y [, angle, sx, sy, ox, oy])`,
`drawFrame(handle, fx, fy, fw, fh, x, y [, angle, sx, sy, ox, oy])`,
`drawRounded(image, x, y, radius)` (corners rounded off, antialiased),
`saveImage(handle, path)`

**Resources** — `release(handle)`, `releaseFont(handle)`, `getStats()`

**Fallible loading** — `loadImage(path)`, `loadFont(size [, path])` — the same
loads, reporting failure as `nil` instead of raising

**Gradients** — `setLinearGradient(x0, y0, x1, y1, stops)`,
`setRadialGradient(cx, cy, radius, stops)`, `clearGradient()`, `hasGradient()`

**Transforms** — `push([mode])`, `pop()`, `origin()`, `translate`, `scale`,
`rotate`, `shear`, `applyTransform`, `replaceTransform`, `getStackDepth()`,
`transformPoint`, `inverseTransformPoint`

**Dimensions** — `getWidth`, `getHeight`, `getDimensions`, `getDPIScale`,
`getPixelWidth`, `getPixelHeight`, `getPixelDimensions`

### Differences from Love2D worth knowing

- Drawables are **integer handles**, not objects: `newCanvas` and `newFont`
  return a handle you pass back to `draw`, `setCanvas`, and `setFont`.
- `newFont(size [, path])` takes the size first. With no path you get the
  host's UI typeface, so text works with no assets to ship; font handle `0` is
  that default face.
- A font file is read whole before its first letter is drawn, once a
  program however many sizes are asked of it. Where `Name.common.ttf` lies
  beside `Name.ttf` (the same face cut down to the characters most text is
  made of, with a tool such as fontTools' `pyftsubset`), Shine starts from
  that file and reads the whole one only when a character outside it is
  drawn: a program with a six-thousand-character face starts as fast as
  one with a few hundred. What is drawn is the same.
- Point lists (`polygon`, `polyline`, `points`) take a flat table of
  coordinates — `{x1, y1, x2, y2, ...}` — instead of varargs.
- Scissor rectangles are transformed by the current transform, so clipping
  composes with `translate` the way nested scroll views need. `getScissor`
  reports the clip in the current drawing coordinates, so `getScissor` then
  `setScissor` restores it exactly under any translate or scale.
- `push()` saves the transform; `push("all")` saves the whole state.
- `newImage(path)` decodes a PNG (via the pure-Rust `png` crate) into the same
  registry canvases live in, so one handle serves both: `draw` composites it,
  `imageSize` measures it, `drawFrame` picks a spritesheet cell out of it, and
  `setCanvas` can even draw into it. `drawFrame` confines sampling to the cell,
  so a magnified frame never pulls in its neighbours.
- Drawable handles are **tagged**, not bare indices. A released handle, or one
  left over from a previous `Window.create`, reports an error instead of
  silently addressing whatever has since taken its slot.
- **Not implemented**: shaders (no GPU), formats other than PNG, and
  `newImageFont` / `newText`.

## Resources have to be released

`newCanvas`, `newImage` and `newFont` allocate; `release` and `releaseFont`
free. Nothing is automatic — the engine cannot see how many Saule values still
hold a handle — so a view that reallocates a canvas when its size changes must
release the old one:

```saule
if cached != nil then
	Graphics.release(cached!)
end

local made: integer = Graphics.newCanvas(width, height)
```

`getStats()` returns live canvases, live fonts, and the bytes their pixels
occupy. Holding those three steady across a few hundred frames is the check
that a frame is not leaking:

```saule
local canvases: integer, fonts: integer, bytes: integer = Graphics.getStats()
```

A release is refused rather than allowed to break something: releasing the
bound render target, the selected font, or a font a `push("all")` state still
names is an error, as is releasing the same handle twice.

## Gradients

A gradient replaces the flat colour as the source for **fills and strokes**;
images and text keep using the colour as their tint. Stops are a flat table of
`position, r, g, b, a` — five numbers each, up to eight stops:

```saule
Graphics.setLinearGradient(0.0, y, 0.0, y + h, {
	0.0, 0.20, 0.22, 0.28, 1.0,
	1.0, 0.10, 0.11, 0.14, 1.0,
})
Graphics.rectangle("fill", x, y, w, h, 6.0, 6.0)
Graphics.clearGradient()
```

The coordinates are local and baked through the current transform when the
gradient is set, so it stays anchored to the shape under a later `translate` —
the same rule scissors follow.

## Loading that can fail

An error raised inside a native call is fatal in Saule and not catchable, so
`newImage` and `newFont` end the program when an asset is missing. `loadImage`
and `loadFont` are the same loads with the outcome as a value:

```saule
local handle: integer? = Graphics.loadImage(path)

if handle == nil then
	return placeholder()   -- missing asset, not a crash
end
```

This replaces the `Io.open`-then-close probe that a caller otherwise needed,
which opened the file an extra time and still raced anything deleting it in
between.

## The `Window` API

**Lifecycle** — `create([width, height, title, resizable])` (1024×600 without a size; opens in the middle of the monitor the mouse is on; the whole screen on Saule Linux), `isOpen()`,
`pollEvents()`, `close()`, `setQuitOnEscape(enable)`, `getQuitOnEscape()`

**Pacing** — `setTargetFPS(fps)`, `getTargetFPS()`, `waitEvents(seconds)`

### The event queue

`pollEvents()` pumps the OS queue and returns everything that happened since
the last call, in order. This is the primary input API — the level queries on
`Keyboard` and `Mouse` answer only what a queue cannot ("is this held *now*").

Each entry is a positional record, `[kind, payload…]`:

| kind | payload |
| --- | --- |
| `keyPressed`, `keyReleased` | `key`, `shift`, `ctrl`, `alt` |
| `textInput` | `text` |
| `mouseMoved` | `x`, `y`, `dx`, `dy` |
| `mousePressed`, `mouseReleased`, `mouseDoubleClicked` | `x`, `y`, `button` |
| `mouseEntered` | `x`, `y` |
| `mouseLeft` | — |
| `wheelMoved` | `dx`, `dy` (notches, positive away from the user) |
| `resized` | `width`, `height` |
| `focusChanged` | `focused` |
| `closed` | — |

The native ABI has no enum-variant type, so the tagged union is rebuilt on the
Saule side. `examples/native-package/keyboard.sau` shows the decoder pattern,
and UIKit ships one in its `Events.sau`:

```saule
for ev: Event in pollEvents() do
	match ev
		case Event.KeyPressed(key, _, ctrl, _) when ctrl then shortcut(key)
		case Event.TextInput(text) then insert(text)
		case _ then nil
	end
end
```

`closed` is delivered **once**, on the transition — not on every poll after
the window has gone. `mouseDoubleClicked` always follows the `mousePressed`
that completed it, so a handler that only cares about single clicks needs no
change; the timing threshold (400 ms, 4 px) lives in the engine so every widget
agrees on it.

**Ordering.** Keyboard messages keep true arrival order — they are recorded in
the backend's input callback as each OS message lands, and modifiers are
captured at that moment rather than read back at the end of the frame. Mouse
and window events are derived per frame, since the backend offers no callback
for them, so within one frame the order is: window changes, mouse motion, mouse
buttons, wheel, then the keyboard log. Motion before buttons is the one that
matters — a click is always delivered against an up-to-date pointer position.

Ignoring the return value is fine: `pollEvents()` still pumps the queue, which
is all a game loop reading held keys needs.

**Geometry** — `getSize()`, `getPosition()`, `setPosition(x, y)`

**Chrome** — `setTitle(title)`, `setTopmost(topmost)`, `isFocused()`

**Density** — `getScale()`

### Shaders

Shine draws images, shapes and text. An effect on top of those (a blur, a
ripple, a wash of colour) is the program's to write, as a shader:

```saule
local invert = Graphics.newShader(String.join("\n", {
    "vec4 effect(vec4 colour, sampler2D image, vec2 uv, vec2 pixel) {",
    "    vec4 was = Texel(image, uv);",
    "    return vec4(1.0 - was.rgb, was.a) * colour;",
    "}",
}))
if invert != nil then
    Graphics.setShader(invert!)
    Graphics.draw(picture, 20.0, 20.0)
    Graphics.setShader()
end
```

`effect` is called for every pixel an image is drawn to while the shader
is set: `colour` is the paint colour, `image` the image, `uv` where in it
the pixel reads (0 to 1) and `pixel` the same in the image's own pixels.
`Texel(image, uv)` reads the image, `texel` is one of its pixels in `uv`,
and `uniform`s the source declares are set with `shader.send(name, x, y,
z, w)`. A source may also have a `void vertex(vec2 uv, vec2 pixel)`,
between `#ifdef VERTEX` and `#endif` with `effect` between `#ifdef PIXEL`
and `#endif`: it runs once a corner and leaves what it works out in the
source's own `varying`s. An effect that reads a pixel's neighbours works
out where they are there, which is what keeps it within what the oldest
chips run (`gpu/shader.rs` has their limits).

`Graphics.capture(canvas, x, y, w, h)` copies what is on the target under
a rectangle into a canvas, made to fit it: what an effect over what is
already drawn starts from. A frosted panel is the two together (uikit's
`Blur`): what is behind it captured small, softened by a shader on its way
down through smaller images and back up, and drawn back.

`newShader` gives `nil` where there is no shader to be had, and
`Graphics.getShaderProblem()` says why: the processor is drawing, or the
graphics chip's own complaint about the source. A program draws without
the effect then.

### Scale

`Window.getScale()` is how many pixels a point of the UI is: a UI lays
itself out in points and multiplies. On a desktop it is the desktop's. On
the bare screen it is what suits the screen (1 below 1000 lines, 1.25 at a
laptop's 1080, 1.5 from 1300, 2 from 2000), or what `Window.setScale(s)`
or `SHINE_SCALE` says; `setScale(0)` goes back to what suits. A window
under a Shine window manager draws at the manager's scale, which the
manager tells it (`Compositor.sendScale`), and starts at it through
`SHINE_SCALE` in its environment.

### Closing

A window closes when it is asked to, and the loop sees `closed`. A program
with something to ask first (unsaved changes) calls `Window.holdClose(true)`:
the request then arrives as a `closeRequested` event with the window still
open, and the program calls `Window.close()` when it is ready. This is
under a window manager of Shine's own; a window on another desktop closes
as that desktop has it.

### Escape does not quit

Love2D ends the loop when Escape is held. This engine does not, unless you ask
for it with `setQuitOnEscape(true)`.

An application toolkit cannot live with the Love2D behaviour: Escape is also
how a modal is dismissed, a menu closed, an autocomplete cancelled — and when
the engine quits on it, the app has no way to decline. `Closed` and the
window's own close button cover the real close.

### Frame pacing

`Graphics.present` is the only thing pacing a Saule game loop. The cap starts
at 60 FPS and `setTargetFPS` changes it; `0` removes it entirely. A mostly idle
UI on battery can drop to 30, and a 120 Hz display can ask for 120.
`Timer.getFPS()` reports what was actually presented, averaged over the last 30
frames — measured at `present`, not from `getDelta`, so a program that never
calls `getDelta` still gets a true number.

A program with nothing to draw has no frame to present and nothing to learn
by going round its loop again a sixtieth of a second later. `waitEvents(seconds)`
ends such a frame in place of `Graphics.present()`: it returns when input
arrives, or after `seconds` (a second at the longest), whichever is first. Input
arriving ends the wait at once, so it is not how late a click is seen. Only a
window on a Saule display server can tell when something has arrived, by
waiting on its connection; everywhere else `waitEvents` is `present()`, and
paces the frame as that does. Anything drawn since the last present is
presented either way.

The mouse's buttons are shown to the loop as levels, looked at once after each
pump of the event queue. On a Saule display server and on the bare screen a
button changes at most once between two looks: a press and its release that
arrive together, because the frame between them was a long one, are a press at
this look and a release at the next, and not a click that never happened.

Windows are resizable by default. The framebuffer is reallocated to match
whenever the window changes size, so `getSize` reports live dimensions and
drawing never lands in a stale buffer; the scissor is reset at the same moment,
since a clip from the old size could sit entirely outside the new one.

`getScale()` returns the display's scale factor — `1.0` at 96 DPI, `1.5` at
150%. minifb has no DPI query, so the engine asks the OS through the native
window handle, and declares the process **per-monitor DPI aware** before the
first window opens. Without that declaration Windows reports 96 DPI and quietly
magnifies the window's pixels, which produces a blurry UI with no way to detect
it. The scale is re-read every `pollEvents`, so moving a window between monitors
of different density is picked up.

Windows and macOS both report a real figure. X11 returns `1.0` rather than
guessing.

**The framebuffer is always physical pixels.** That is the rule that makes the
rest consistent: `getSize` and `Mouse.getPos` are in framebuffer pixels on
every platform, and an app divides by `getScale()` to work in logical units.

On macOS this takes a conversion, because minifb works in *points* there. The
engine asks the `NSWindow` for its `backingScaleFactor` and allocates the
framebuffer that much larger — a 400×200 window on a Retina display gets an
800×400 buffer, and pointer coordinates are scaled to match. minifb's macOS
backend presents the framebuffer as a Metal texture on an `MTKView` whose
drawable is already sized in physical pixels, so this makes the mapping 1:1
rather than a 2× upscale.

It matters most for text. A glyph rasterized at 13px and magnified by the OS is
soft and visibly pixelated; rasterized at 26px into a 2× buffer it is sharp.
Build fonts at `size * getScale()` and scale the root transform to match — the
same thing `TextStyle` in the UIKit example does.

## The `Mouse` API

Motion, buttons and the wheel arrive as events (see above); what stays here is
the state a queue cannot answer.

**Position** — `getPos()`, where the pointer is right now.

**Buttons** — `isDown(button)`, whether it is held right now. `1` = left,
`2` = right, `3` = middle.

**Cursor** — `setCursor(style)`, `setVisible(visible)`. Styles are `"arrow"`,
`"ibeam"`, `"crosshair"`, `"hand"`, `"grab"`, `"resizeleftright"`,
`"resizeupdown"`, `"resizeall"`; an unknown name is an error rather than a
silent no-op.

**A trackpad's own feel** — `setTouchpadSpeed(speed)`,
`setTouchpadScrollSpeed(speed)`, `setTouchpadNaturalScrolling(on)`,
`setTapToClick(on)`: beside `setPointerSpeed` and the rest, which are a
mouse's. They take effect on the bare screen, where Shine reads the
trackpad itself; in a window the desktop around has already made of the
fingers what it makes.

On the bare screen Shine makes a touchpad's fingers into movement itself
(`state/console/touchpad.rs`): each finger is followed by itself, so one
landing or lifting moves nothing; a reading that wanders under a still
finger, or leaps as a finger lifts, is not movement; a faster finger goes
further; a tap clicks, and a touch straight after a tap holds the button
until it lifts, which drags. Two fingers scroll, wherever they are on the
pad and however they lie to each other, and the pointer keeps out of it:
a touch that has had two fingers on it scrolls until the last one lifts,
and beside a finger that may be a thumb or the lower of two scrolling
nothing moves until it is known which. A scroll let go of while moving
carries on, slowing, until it has run down, the pad is touched or a key
is pressed. A pad that is its own button takes a finger
that came down along its bottom edge and stays there for a thumb waiting
to press, not a second finger, and its bottom right for the secondary
button.

A mouse moved fast takes the pointer further than the same distance
moved slowly, up to twice as far (`pointer::quickened`), under
`Mouse.setPointerSpeed`; a wheel that turns smoothly scrolls by the parts
of a notch it says. `Mouse.setAcceleration(on)`,
`Mouse.setTapAndDrag(on)` and `Mouse.setTouchpadInertia(on)` turn the
quickening, the drag after a tap and the scroll that carries on off and
on; each is on unless turned off.

**`Devices.list()`** — what the machine is typed on and pointed at with,
each as `"kind<TAB>where<TAB>name"`: kind is `keyboard`, `mouse`,
`touchpad`, `touchscreen` or `tablet` (a pointer that says where it is,
not how far it moved); where is `built-in` or `connected`. It follows
devices being plugged in and taken out. On the bare screen only; in a
window the list is empty, and the desktop around has the devices.

Input is sampled after *both* of minifb's message pumps — `update` in
`pollEvents` and the one inside `update_with_buffer` in `present` — and the
extra edges are carried into the next frame. Each pump begins by resetting
minifb's own scroll delta and key tracking, so anything that arrived during the
present half of a frame used to be wiped before Saule could read it: roughly
half of all wheel notches and any keystroke short enough to start and finish in
that window. The keyboard does the same thing for the same reason.

## The `Clipboard` API

`get()`, `set(text)`, `hasText()` — plain text only. `get()` returns `""` when
the clipboard is empty or holds something that isn't text, so a paste handler
needs no error handling.

Backed by `arboard` with default features off, which keeps it pure Rust: x11rb
on Linux, `windows-sys` on Windows. The connection is kept alive for the life of
the process because on X11 the clipboard is *owned* by a live connection —
dropping it would take the copied text with it.

## The `Keyboard` API

Love2D splits keyboard input between level queries and callbacks
(`love.keypressed`, `love.keyreleased`, `love.textinput`). Saule owns the loop
here, so the callbacks become `KeyPressed` / `KeyReleased` / `TextInput` events
in the queue, and what remains on `Keyboard` is the level half.

**Held keys** — `isDown(key)`, `isAnyDown({key, ...})`, `getKeysDown()`

**Key repeat** — `setKeyRepeat(enable)`, `hasKeyRepeat()`

**Text gate** — `setTextInput(enable)`, `hasTextInput()`. Off means no
`TextInput` events are produced; disabling it mid-frame also drops text already
typed this frame, so a handler never sees input the app has just declined.

**Layouts** — `setLayout(layout, variant)`, `getLayout()`, `layouts()`. On
the bare screen the kernel reports which key was pressed, not what is
printed on it; what it types is a layout's to say. The layouts are XKB's
(`"lt"`, `"de"`, `"us"`; `variant` `""` for the usual one), read by
libxkbcommon, which is looked for when a layout is asked for and not
linked: without it, or without XKB's descriptions, `setLayout` returns
false and the keyboard types as a US one. `layouts()` lists what there is
to choose from, each `"code<TAB>Name"`. `XKB_DEFAULT_LAYOUT` chooses one
before the program asks. Dead keys compose (`´` then `e` is `é`), AltGr
picks a key's third character and is then not reported as Alt, and key
*names* stay the key's position, so a shortcut is the same keys in every
layout. In a window the desktop's own layout types and these do nothing.

**Losing the keyboard.** Under a window manager of Shine's own, a window
told it no longer has the keyboard lets go of every key it held: the news
of their release goes elsewhere from then on, and a key never released is
a shortcut's modifier held for good.

Key names are Love2D's `KeyConstant` strings — `"a"`, `"space"`, `"lshift"`,
`"return"`, `"kp0"`, `"/"` — so code reads the same as its Love2D equivalent.
Unrecognised names report as "not down" rather than erroring. See
`examples/native-package/keyboard.sau` for a text field and WASD movement built
on the whole surface.

### Differences from Love2D worth knowing

- The callbacks are a queue, not per-frame flags. A key tapped and released
  between two polls still reports both edges, and two taps of the same key in
  one frame stay two events — neither survives a level-diffing API.
- `isAnyDown({"lshift", "rshift"})` replaces Love2D's variadic
  `isDown(key, ...)`, since native calls take a fixed argument list.
- `TextInput` carries text the way `love.textinput` would deliver it — layout-
  and modifier-aware, so shift+`a` arrives as `"A"`. A run of characters
  coalesces into one event, capped at 4 KiB.
- `setKeyRepeat(true)` makes a held key keep emitting `KeyPressed` after a
  0.25 s delay, then every 0.05 s. Repeats are synthesised by the engine, so
  they land after the frame's real messages.
- On the bare screen the keys with pictures on them are events too, under
  these names: `volumeup`, `volumedown`, `mute`, `brightnessup`,
  `brightnessdown`, `kbdillumup`, `kbdillumdown`, `micmute`, `audioplay`,
  `audionext`, `audioprev`, `printscreen`, `sleep`, `power`. They repeat
  while held, as the kernel repeats them, and `isDown` does not know them.
- **Not implemented**: scancodes (`isScancodeDown`, `getScancodeFromKey`,
  `getKeyFromScancode`). The windowing backend reports keys already mapped
  through the OS layout, so there is no honest physical-position code to hand
  back.

## Text

Glyphs are rasterized on demand by `fontdue` and cached per font object, so a
label costs one rasterization on the first frame and a blit on every frame
after.

**Fallback.** One face never covers everything — a UI font has no CJK, and
almost nothing outside a colour-emoji font has emoji. Each font keeps a lazily
loaded chain of host faces and consults it for any codepoint its primary face
lacks, so a missing glyph draws real text instead of a blank box. The chain
loads on the first character that actually needs it, so a Latin-only UI never
pays for it.

**Line breaking.** `printf` breaks after spaces *and* between wide characters —
CJK, kana, hangul, emoji. Scripts written without spaces used to form a single
unbreakable "word" per paragraph and never wrapped at all. Opening and closing
brackets stay attached to their neighbour, so a line never begins with `。` or
ends with `「`. A Latin word longer than the limit is still left overlong on its
own line rather than split, matching Love2D.

**Justification.** `printf(..., "justify")` distributes the slack across the
gaps between words. The last line of a paragraph is left alone, since
stretching it would spread a short final line edge to edge.

## Building & installing

```sh
cargo build --release
```

Then copy the library into your Saule home — the file name does not matter,
Saule reads the package's real name out of it:

```sh
# Linux
cp target/release/libsaule_shine.so    ~/.saule/native_packages/
# macOS
cp target/release/libsaule_shine.dylib ~/.saule/native_packages/
# Windows (PowerShell)
copy target\release\saule_shine.dll $env:USERPROFILE\.saule\native_packages\
```

That is the whole install. Nothing needs regenerating after you change a
`#[saule_export]` signature: the package's description is compiled from the
same declarations as its code, so the two cannot drift.

### macOS: one extra step

Apple's linker (`ld-27037.1`, Xcode 26) sometimes leaves a release build's
symbol string table 4-byte aligned, and macOS 27's dyld then refuses to load
the library at all:

```
mis-aligned LINKEDIT string pool, fileOffset=0x000FAEE4
```

It is a linker bug, not anything about this code — it happens when the indirect
symbol table has an odd number of entries, so whether a build is affected
changes with any edit. Repair the library after building it with
[`scripts/align_macho_strtab.py`](https://github.com/lauriszz123/saule/blob/develop/scripts/align_macho_strtab.py)
from the Saule repository:

```sh
python3 align_macho_strtab.py target/release/libsaule_shine.dylib
```

It pads the string table, re-signs ad-hoc, and does nothing to a library that
came out aligned, so it is safe to run every time. Saule says so explicitly if
you install one that still needs it.

## Toolchain compatibility

The native ABI version is checked when the library loads, so a build made
against one Saule toolchain is refused by a mismatched one, with both versions
named. `Cargo.toml` tracks the Saule repository's `develop` branch while the
SDK is unpublished; pin a tag there before cutting a release.
