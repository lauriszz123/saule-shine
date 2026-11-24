# Drawing on the GPU

Shine drew everything on the CPU (`src/raster`, `src/render`). This is how it
moves onto the GPU without losing what the CPU renderer gives: exact, tested
pixels, and a program that runs on a machine with no GPU at all, which still
draws with the CPU renderer.

## Which interface

**OpenGL ES 2.0**, or desktop **OpenGL 2.1** where ES isn't offered (Windows'
WGL, macOS's CGL), one set of shaders for both. It is the newest interface
every target has. The netbook Saule Linux is built for has an Intel GMA 3150,
which stops at ES 2.0; wgpu needs ES 3.0, so it would leave that machine on
the CPU. Contexts come from glutin on the window minifb opened (EGL/GLX on
X11, WGL or ANGLE on Windows, CGL on macOS) and, on Saule Linux's bare screen,
from EGL on GBM through Mesa (see saule-linux's driver packs).

## Steps

Each step keeps the CPU path and is checked against it pixel for pixel.

1. **Present** — done (`src/gpu/present.rs`). A frame goes up as a texture
   (or is already one, when the GPU drew it) and is drawn over the window,
   then the buffers swap. Falls back to X11 shared memory / minifb when no
   context can be made.
2. **GPU surfaces** — done (`src/render/mirror.rs`). Every surface the GPU
   touches gets a texture twin, and each side knows which copy is current:
   the GPU uploads before it reads a surface the CPU changed, the CPU reads
   back before it uses one the GPU drew. So an operation without a GPU path
   still sees exactly the GPU's pixels. `render/gpu_tests.rs` draws every
   scene both ways and compares every pixel (within 3/255), and checks the
   last thing in each was drawn on the GPU.
3. **Images and windows** — done. `clear`, `draw`, `drawFrame` and
   `drawRounded` with any transform, tint, scissor, rounded corners, linear
   or nearest filtering, and every blend mode (alpha, add, subtract,
   multiply, screen; replace where nothing is partly covered). A window's
   frame in a display server goes straight from the app's shared memory
   into its texture (`Renderer::upload_frame`), the CPU's copy brought back
   only if something reads it.
4. **Shapes** — done. An axis-aligned rectangle (most of a UI) is filled
   entirely on the GPU, its coverage worked out in the shader as the CPU's
   rectangle path works it out; any other shape's coverage comes from the
   CPU's own scanline filler (`raster::coverage_mask`) as a one-byte-a-pixel
   mask, so its edges are the CPU's to the pixel and only the blending
   moves. That coverage is kept in a shape atlas, keyed by the outline
   relative to its pixel, so the buttons and cards drawn every frame cost
   no coverage pass and no upload after the first. Linear and radial
   gradients (up to eight stops) are evaluated in the shader.
5. **Text** — done. Glyphs are rasterized by the CPU as before (fontdue,
   cached per font) and kept in a glyph atlas, each with an empty border; a
   line at whole pixels (nearly all text) is one draw, and a scaled or
   turned glyph is sampled as the CPU samples it.

   **GPU drawing is on by default from here** (`SHINE_GPU_DRAW=0` turns it
   off). A frame of the window manager or gamemaker runs entirely on the
   GPU — no copy between the two sides (`SHINE_TRACE_FRAMES=1` counts them)
   — and looks identical to the CPU's, pixel for pixel. The Shine bench
   (rounded rows and text, 1024×600, 30 frames): 55–73 ms on the GPU
   against 70–83 on the CPU, on a fast desktop; the difference grows with
   larger and more translucent drawing, and on slower CPUs.

   Still on the CPU: the replace blend mode where shapes or glyphs are
   partly covered, and pixel-by-pixel image edits (`setPixel`, flood fill).
6. **The bare screen**: EGL on GBM for the console backend, page-flipping
   GBM buffers instead of dumb buffers.
7. **Apps in a display server** — done. An app inside the window manager
   has no window to make a context on, so it makes one on no surface
   (`Device::headless`: EGL's device platform, the GPU where it will start)
   and draws on the GPU like any other program, from its first frame.

   Its frames reach the server without a copy where the driver can share
   GPU buffers (`gpu::dmabuf`): the app shares two textures as dma-bufs
   (`EGL_MESA_image_dma_buf_export`, sent once per size with
   `attachShared`), copies each frame into the next of the two on the GPU
   and names it (`commitShared`); the server imports them
   (`EGL_EXT_image_dma_buf_import`) and uses them as the window's image as
   they are. Working on Saule Linux in QEMU (virgl, `make GL=1 run-netbook`)
   and the way it will work on the netbook's i915.

   Where the driver can't (WSL's D3D12), or the server refuses, the app
   says so once (with `SHINE_TRACE_PRESENT=1`) and reads each frame from
   its texture into the shared memory instead, which the server uploads:
   one read and one upload, still with no CPU drawing on either side.
8. **Batching** — done for shapes. Rectangles and shapes from the shape
   atlas in a flat colour, drawn one after another onto the same target in
   the same blend mode, are gathered (`gpu::batch`) and drawn in one call,
   everything a shape needs carried by its vertices. Anything else the GPU
   is asked to do draws the batch first, so the order never changes. A line
   of text was one draw already. 150 rounded, see-through cards a frame:
   150 draws before, 1 now.

### Measured on Saule Linux in QEMU

`examples/gpu-load` in saule-linux (a gradient, 150 moving see-through
rounded cards, 12 lines of text and a turning image every frame, in the
window manager at 1016×564), 2 virtual CPUs with KVM, virgl on the host's
GPU. The guest's CPU time is the host time of QEMU's two vCPU threads:

| Drawing                          | App fps | Guest CPU (of one core) | CPU per frame |
|----------------------------------|---------|-------------------------|---------------|
| On the CPU (`SHINE_GPU_DRAW=0`)  | 30      | 59%                     | ~20 ms        |
| On the GPU, frames copied        | 59      | 28%                     | ~4.7 ms       |
| On the GPU, frames shared        | 55      | 24%                     | ~4.4 ms       |

What is left on the guest's CPU is the program itself (moving 150 cards
in Saule), laying out text, and handing GL commands to the virtual GPU.

Still to do: the app waits for its frame to be finished (`glFinish`)
before naming it to the server, which under virgl is a round trip to the
host — 13 ms of waiting (idle, not busy), so 55 fps rather than 60. A fence
passed with the frame (`EGL_ANDROID_native_fence_sync`) would let it go on
at once.

## Switches

- `SHINE_GPU=0` — no GPU: frames go through shared memory or minifb.
- `SHINE_GPU_DRAW=0` — present through the GPU, but draw on the CPU.
- `SHINE_TRACE_PRESENT=1` — which way frames go, and which GPU.
- `SHINE_TRACE_FRAMES=1` — frame times, copies between CPU and GPU, and
  shapes per batched draw.
- `SHINE_SHARE_FRAMES=0` — an app in a display server sends its frames as
  pixels even where it could share its GPU buffers.
- Under WSL, Shine asks Mesa for its D3D12 driver (`GALLIUM_DRIVER=d3d12`)
  unless `GALLIUM_DRIVER` is set: Mesa otherwise draws on the CPU there.
