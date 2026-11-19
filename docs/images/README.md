# README visuals

This directory holds a diagram of the local pool model, a recording of a reader
arriving after its writer exits, and a recording of the browser map.

## Local IPC recording

`ipc/local-ipc.gif` records the 2.0 command syntax in an actual shell.
The writer finishes before the reader starts. The reader exits after the
first retained message.

`ipc/local-ipc.cast` contains the terminal's original bytes and timings, the
version reported by `pls` and the SHA-256 of the `pls` executable on `PATH`.
`ipc/local-ipc.png` is the final still. The recording measures 1220 × 370 pixels.
The renderer uses the terminal bytes, without inventing or replacing output.

After the 2.0 release, install Plasmite 2.0.0 on macOS and run from the repo root.

```bash
uv run scripts/record_readme_demo.py --font /System/Library/Fonts/Menlo.ttc
```

The script looks for `pls` on `PATH` and requires a monospace TTF/TTC font.
On Linux, pass the path to a font installed there.
It creates its own temporary pool directory and removes it when finished.
It checks the installed version and reads the retained message back before
writing the assets. For a 2.0 candidate, add `--candidate` and put that build on `PATH`.

## Pool diagram

`ipc/pool-model.svg` is an editable diagram of two writers and two independent
readers sharing one local file. The JSON boxes stand for retained messages;
they do not describe the file's byte layout or capacity overhead. Reading
does not remove a message, and new writes overwrite old history when full.

## Browser map

`ui/pool-map.gif` shows sample events in four pools, message previews, and
hover highlights on the spiral. `ui/pool-map.png` is frame 120 of that existing
recording, for readers who prefer a still image. The GIF remains unchanged.

The older `ui-pool-list.png` and `ui-pool-follow.png` show earlier UI layouts.
They are not featured in the README. Capture the current released UI before
using those views in new documentation.
