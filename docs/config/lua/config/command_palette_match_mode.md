---
tags:
  - command_palette
---
# `command_palette_match_mode = "Fuzzy"`

{{since('nightly')}}

Controls how the text you type into the command palette
([ActivateCommandPalette](../keyassignment/ActivateCommandPalette.md)) is
matched against the available commands. Possible values are:

* `"Fuzzy"` (the default) - the typed text is treated as a fuzzy pattern;
  characters must appear in order but need not be contiguous. Multiple
  whitespace-separated words are matched as independent fuzzy atoms.
* `"Exact"` - each whitespace-separated word must occur verbatim
  (case-insensitively) somewhere in the entry, but the words may appear in any
  order. For example, `tab new` matches an entry containing both `new` and
  `tab` regardless of their order.

Regardless of this setting, you can toggle between the two modes while the
palette is open by pressing `CTRL-R`. The currently active mode is shown on the
right of the input line.

The matched portions of each entry are highlighted using
[command_palette_match_color](command_palette_match_color.md).

```lua
config.command_palette_match_mode = "Exact"
```
