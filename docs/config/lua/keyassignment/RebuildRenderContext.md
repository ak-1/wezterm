# `RebuildRenderContext`

{{since('nightly')}}

Tears down the GPU render context for the current window and creates a
fresh one, re-creating all GPU-allocated resources (glyph atlas, shaders,
vertex buffers) along the way.

This is useful to recover a window whose rendering has become corrupted
after something outside of wezterm's control invalidated the driver-side
GPU state without reporting the context as lost. The most common trigger
is restoring a virtual machine from a saved state while wezterm is
running inside the guest: the whole window may afterwards render with
washed out, low-contrast colors until the context is rebuilt.
Suspend/resume cycles with some drivers can produce similar effects.

This action currently applies only to the OpenGL front end; it is
ignored when `front_end = "WebGpu"`.

It is available from the command palette, or can be bound to a key:

```lua
config.keys = {
  {
    key = 'F5',
    mods = 'CTRL|SHIFT',
    action = wezterm.action.RebuildRenderContext,
  },
}
```
