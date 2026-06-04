# `PasteImageFrom(source)`

{{since('nightly')}}

Pastes an **image** from the clipboard into the current pane.

Many terminal CLI tools (for example [Claude
Code](https://docs.anthropic.com/en/docs/claude-code)) can load an image when
they are given the *path* to an image file. `PasteImageFrom` bridges the
clipboard to those tools: when the clipboard holds an image, wezterm writes it
to a temporary `.png` file **on the host where the pane's program is running**
and then pastes that file's path into the pane.

This is a "smart paste": if the clipboard does **not** contain an image, it
behaves exactly like [`PasteFrom`](PasteFrom.md) and pastes text instead.

`source` takes the same values as [`PasteFrom`](PasteFrom.md):

* `Clipboard` - the system clipboard
* `PrimarySelection` - the primary selection buffer

It has no default key assignment; bind it yourself:

```lua
local wezterm = require 'wezterm'
local act = wezterm.action

config.keys = {
  -- Smart paste: an image becomes a file path, anything else is pasted as text
  { key = 'V', mods = 'CTRL|SHIFT', action = act.PasteImageFrom 'Clipboard' },
}
```

## Why this exists: remote / multiplexer sessions

When you attach to a remote multiplexer — for example `wezterm connect
SSHMUX:host` — the program in the pane runs on the **remote** machine, while the
clipboard lives on your **local** machine. Tools that read the OS clipboard
directly (the usual way `Ctrl+V` image paste works locally) therefore cannot see
your local clipboard over the connection, and image paste silently fails.

`PasteImageFrom` solves this by sending the image over the multiplexer protocol:

1. wezterm reads the image from your local clipboard.
2. The bytes are sent to the multiplexer server hosting the pane.
3. The server writes them to a temporary file **on the remote host** (a
   space-free name such as `/tmp/wezterm-paste-<pane>-<timestamp>.png`, so that
   path detection is not defeated by quoting).
4. The server pastes that remote path into the pane, where the program can load
   it from its own filesystem.

Because delivery rides the multiplexer protocol, it works the same way for
local panes, unix-socket domains, SSH multiplexer domains (`SSHMUX:`), and TLS
domains. For a local pane the temporary file is simply written on the local
machine.

!!! note
    Plain `Ctrl+V` is intentionally **not** rebound — it is left to pass
    through to the program in the pane (which is how local image paste already
    works for many tools). Bind `PasteImageFrom` to a separate key such as
    `CTRL|SHIFT V`.

## Platform support

Reading an image from the clipboard is currently implemented for the **Wayland**
backend only. On other backends (X11, macOS, Windows) the clipboard image read
reports that it is unsupported and `PasteImageFrom` falls back to a normal text
paste. Delivery of the image to the pane's host is platform independent.

## Related work

A different, broader approach to the same problem is proposed upstream in
[PR #7624](https://github.com/wezterm/wezterm/pull/7624) ("Add clipboard image
paste support with Ctrl+V smart paste for all platforms"), building on
[issue #7272](https://github.com/wezterm/wezterm/issues/7272). That work adds
cross-platform clipboard-image reading (X11, Wayland, macOS, Windows) and a
`PasteImageToSshUpload` action that uploads the image to a remote host over
SFTP/SCP.

The key difference: `PasteImageToSshUpload` targets plain SSH domains
(`RemoteSshDomain`) or an `ssh` process detected in the pane's process tree, and
does not cover multiplexer (`ClientPane`) sessions such as `SSHMUX:`.
`PasteImageFrom` instead delivers the image over the multiplexer protocol, which
covers exactly those multiplexer sessions and is transport-agnostic.

## See also

* [`PasteFrom`](PasteFrom.md) - paste text from the clipboard
* [`Paste`](Paste.md)
